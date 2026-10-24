# The transcript and asks

> How RimZ records a conversation and reads it back: the durable transcript log, its entry kinds and their authors, the hook-side writers, the causal links between entries, the `rimz transcript` projection, and the ask records that turn a blocked agent's question into structured data. How text reaches a pane in the first place is [messaging.md](./messaging.md). For users, the commands are [cli/transcript.md](../../reference/cli/transcript.md) and [cli/asks.md](../../reference/cli/asks.md).

| Code | Owns |
| --- | --- |
| [`transcript.rs`](../../../crates/rimz/src/transcript.rs) | The log: `TranscriptEntry`, the read-side `EntryOrigin` projection, the write-side `SectionOrigin` mapping, bucket I/O, read windows, the legacy filter |
| [`store/message.rs`](../../../crates/rimz/src/store/message.rs) | `classify_submitted_prompt`, which splits a submitted prompt into authored sections |
| [`cli/hooks/lifecycle/transcript.rs`](../../../crates/rimz/src/cli/hooks/lifecycle/transcript.rs) | The hook writers: conversation entries, Cursor's response, native answers, turn errors |
| [`cli/transcript/`](../../../crates/rimz/src/cli/transcript) | The reader: scope (`scope.rs`), harness-turn hiding and thread assembly (`thread.rs`), rendering (`chat.rs`, `ask_card.rs`), follow mode (`follow.rs`) |
| [`agents/open_ask.rs`](../../../crates/rimz/src/agents/open_ask.rs) | Ask materialization: questions, ask-time context, and the answer route |
| [`cli/asks.rs`](../../../crates/rimz/src/cli/asks.rs), [`cli/answer.rs`](../../../crates/rimz/src/cli/answer.rs) | `rimz asks` and `rimz answer` |

## The log

The transcript is a RimZ-owned conversation log, separate from each provider's native session files. Ended agents, past channels, and their asks and answers stay readable after the native files rotate away. `transcript::append` writes one JSONL line under the workspace lock to a seven-day bucket at `audit/transcript/<bucket-start>.jsonl` in the workspace state directory. `disk/buckets.rs::bucket_file_name` names each bucket by the UTC date that starts its epoch-aligned window, and the width is `disk::retention::TRANSCRIPT_FILE_DAYS`, which message history shares. The files are append-only. Readers sort by the recorded `at`, so a bucket boundary carries no ordering meaning.

[`gc.transcript_retention`](../../reference/cli/config.md#set-a-value) governs the transcript and message history together (`store/gc/classes.rs::collect_transcript`). The default is `forever`. A positive duration expires whole files by mtime, so a bucket goes only after its last append ages out, and its entries can outlive the setting by up to seven days. The audit sweep's age rule and size cap skip both directories (`collect_audit`). A hard reset removes the buckets, and so does `rimz uninstall --state` or `--all`. A plain uninstall keeps them.

Routine reads use a 30-day floor, `transcript::recent_floor`. `read_since` selects buckets by file name before it opens any, newest first, so the bucket that overlaps the floor can hold entries up to seven days older than the floor. An ask or answer older than the window takes no part in lookup or duplicate suppression.

| Reader | Window |
| --- | --- |
| `rimz transcript`, `rimz agents logs`, and their follow polls | 30 days. `--all` reads every bucket (`read_all`) and includes the prior-session archive |
| Channel discovery (`transcript::channels`, `workspaces_with_channel`) | 30 days |
| Open-ask lookup, ask materialization, answer deduplication, `rimz answer` confirmation | 30 days |
| `rimz agents wait`, for a turn's final message (`latest_assistant`) | Buckets from the turn's start |
| Hook redo and replay deduplication | Buckets from the ingress frame's timestamp |
| `rimz agents attribution` | Every bucket |

A windowed view that skipped older buckets says so. It sets `older_omitted` in non-follow JSON, or prints a faint stderr hint that names the `--all` command (`cli/transcript/mod.rs::write_older_history_hint`), even when the selected scope is empty. The flag reports that the workspace has older files, not that the scope has older entries, since the skipped files are never opened.

Hook redo and replay deduplication read from the ingress frame's timestamp (`hook_drain::frame_timestamp`) instead of the floor. An earlier apply attempt stamped its entries at apply time, at or after ingress, so the buckets from the frame's timestamp hold them. A redo matches its earlier copy by entry kind and `ingress` key. This assumes the host clock does not step backward across a bucket boundary between ingress and apply.

`read_bucket` is the one decode point, and it drops legacy paste fragments (`is_legacy_paste_fragment`). A fragment is a `Prompt` with no `from` and no `message_id` whose whole trimmed text is one `<pasted_content id="X">` or `</pasted_content id="X">` tag line. Its doc comment records them as written by RimZ 0.4.3 and earlier, before the prompt sanitizer peeled Claude Code's paste envelope. The test is unconditional and runs on every entry read, so a human prompt whose entire text is one such tag line is dropped too. Matching a whole entry exactly is the only form safe to apply to text RimZ never normalized, and that loss is its accepted cost. A genuine prompt with a wrapper pair mid-text has several lines and never matches. The same decode reads the legacy `from: "you"` as `"user"`. No reader rewrites the files.

Three nearby reads use other sources. Supervised-run streaming tails the provider-native transcript through the adapter-owned source ([scripting.md § Output and input projections](./scripting.md#output-and-input-projections)). The context and spend gauges read the provider's own sources ([model.md § Enrichment](../agents/model.md#enrichment)). The message audit trail in the event log carries no text ([messaging.md § Storage and audit](./messaging.md#storage-and-audit)).

## Entry kinds

`TranscriptKind` has eight variants.

| Kind | Records | Human rendering |
| --- | --- | --- |
| `Prompt` | A human prompt, or a headerless system prompt or command with `from: "rimz"` | `user: @receiver, text` for a human prompt. A system prompt or command is hidden with the output of the turn it opens |
| `Message` | An inter-agent delivery or a launched child's brief, with the sender's handle as `from` | `@sender: @receiver, text` |
| `SubagentReport` | A fleet digest, agent report, or team report, with the header's sender as `from` (`@rimz`, or `@<child>` on a legacy per-child report) | Hidden with the output of the turn it opens |
| `Wait` | A harness notice: a `WAIT`, `SIGNAL`, `STAGE`, `CACHE_KEEPALIVE`, or `DEADLINE` header, or a record-backed paused-child, stalled-child, queued, or undelivered notice | Hidden with the output of the turn it opens |
| `Assistant` | A root turn's final assistant message | `@receiver: text` |
| `Ask` | A blocking question or plan approval with parsed questions, or a Codex async question | The agent's question |
| `Answer` | The effective answer: from the native prompt UI, from `rimz answer`, or the first direct human prompt submitted while a question is open | `user` to the agent, folded into its ask card. An orphan uses its recorded question as an answer card, or a plain body when no question text was recorded |
| `Error` | A hook-decoded provider turn error newly merged into `AgentContext.turn_error` | `@receiver: error text`, styled as an error |

`rimz transcript --json` includes every hidden entry. [`TranscriptEntry::origin`](../../../crates/rimz/src/transcript.rs) is the one read-side origin decision, shared by rendering, thread assembly, and attribution. It derives `EntryOrigin` from `(entry, from)` with no stored origin field, and the first matching row wins:

| Kind | Stored `from` | Origin |
| --- | --- | --- |
| `SubagentReport`, `Wait` | Any | `Harness` |
| `Assistant`, `Ask`, `Error` | Any | `Agent` |
| Any remaining kind | `"rimz"` (`HARNESS_FROM`) | `Harness` |
| `Message` | Absent | `Harness` |
| `Prompt`, `Answer` | Absent | `Human` |
| Any remaining kind | `"user"` (`HUMAN_FROM`) | `Human` |
| Any remaining kind | Any other sender | `Agent` |

Answers record `from: "user"`, and typed human prompts carry no `from`. `is_harness()` delegates to the projection, and conversation counts count only human-origin prompts as input from the user. Every human-origin entry renders as `user`. Stage lines take their origin from the flip's author instead: the operator is human, and any other author is an agent.

## Writing entries

The write side has one classifier and one inverse. `classify_submitted_prompt` gives each section of a submitted prompt a `SectionOrigin`, and `SectionOrigin::entry_kind_and_from` turns it into the entry kind and `from` that `TranscriptEntry::origin` reads back. A round-trip unit test pins that only a human section reads back as the user. Human is the residue: text becomes the user's only when no header, record, or run accounts for it, so RimZ-introduced text never reads as the user's for lack of a sender.

### Order in the turn-start hook

The receiver's turn-start hook (`cli/hooks/lifecycle/mod.rs`) runs these steps in order:

1. Record the lifecycle event and fold it into the snapshot, inside the hook transaction.
2. Resolve the turn's run: the supervised run named in the environment (`harness::launch::ENV_RUN_ID`), else the row's open peer or team run (`session_run_id`).
3. Read this card's in-flight records through `Store::list_messages`, then confirm delivery. In-flight means `Sent` or `awaiting_late_ack()`, wider than the queued-only pending list.
4. Resolve the run again when step 2 found none, so a turn that the delivery just claimed is seen.
5. Classify the submitted sections once, against the confirmed and in-flight records.
6. Record spend user input, then write the transcript entries (`record_conversation`).

The fold in step 1 runs before any record is read, so it uses the record-less classifier (`prompt_is_harness_delivered`). A headerless system prompt can therefore restart `user_turn_started_at`. Spend and transcript consume the same record-aware sections after confirmation. Spend counts a human section only when its record, if any, passes `is_user_input()`, which excludes automated and `Resume`-gated records, and only when the turn carries no run. Classification changes no message status: `TurnStarted` confirms only prompts, and `Compaction` only commands.

### Section classification

A batched delivery splits at each blank line that is followed by a recognized header line (`split_batched_prompt`), so each section becomes its own entry. When the confirmed records align with the paste (`align_submitted_prompt`), the record boundaries win over the split. Each section then parses the [message header](./messaging.md#the-message-header) once.

| Header `Type` | Entry |
| --- | --- |
| `AGENT_MESSAGE` | `Message`, with the header's sender as `from` |
| `AGENT_REPORT`, `TEAM_REPORT`, or `SUBAGENT_REPORT` | `SubagentReport`, with the header's sender as `from` |
| `WAIT`, `SIGNAL`, `STAGE`, `CACHE_KEEPALIVE`, or `DEADLINE` | `Wait`, with the header's sender as `from` |
| `USER_MESSAGE` | `Prompt`, header removed, no `from` |
| No recognized header, and a confirmed or in-flight record accounts for it | The record sender's origin: human is `Prompt` without `from`, agent is `Message` from its handle, a child or report is `SubagentReport`, any other harness notice is `Wait`, system is `Prompt` with `from: "rimz"` |
| No recognized header, and nothing accounts for it | Human `Prompt`, subject to the cases below |

A header's origin wins over a matched record's sender. An unaligned section matches a confirmed record by exact body text, in order. A section with no header can also match an in-flight record, but only one that ships headerless (a system prompt or any command) with the same trimmed text, and each record matches once. A system `/compact` is therefore harness-authored even before its compaction acknowledgement, while an operator's command keeps that operator's human or agent origin.

Three cases change the mapping:

- While the agent has an open blocking question, the first direct human `Prompt` section becomes that ask's id-stamped `Answer`. An attributed queue record never answers a question.
- A record-less human section that matches the turn's run prompt takes the run's origin ([run launch prompts](#run-launch-prompts)).
- Text around a confirmed headered batch becomes separate direct-input `Prompt` entries ([messaging.md § Confirmation and retry](./messaging.md#confirmation-and-retry)).

### Run launch prompts

A record-less human section on a turn with a run can be that run's launch prompt. The writer loads the run once per turn and compares the section with `RunRecord.prompt`, after `agents::peel_rimz_blocks` removes registered RimZ wrappers from both sides. `RunRecord::prompt_origin` decides the origin:

| Run | Origin | Entry |
| --- | --- | --- |
| Subagent, peer, or team run | `Parent` | `Message` from the parent's or launcher's handle. An unresolved launcher gives a harness `Prompt` with `from: "rimz"`, never the user |
| Any other run with a `loop_task` | `Harness` | `Prompt` with `from: "rimz"` |
| Any other run | `Human` | Human `Prompt` |

A retry carries the same `subagent` and `loop_task` fields, so it keeps its origin without loading `retry_of`. Text that does not match the run prompt follows ordinary classification.

Persistent peers and team seats carry no run id in their environment, so step 2 finds their open run from the row (`harness::run::open_peer_run`, `open_team_run`). An open run suppresses the human-input spend window, as a supervised run does. Once the run settles, later turns carry no run: even identical launch text is then human-authored and opens a spend window.

An agent-launched interactive peer gets its run before its prompted pane opens (`cli/agents_cmd/launch.rs::prepare_peer_prompt`), so its launch prompt has launcher origin from the first hook. `rimz message --create` from an agent takes the same path. User launches and promptless or resumed launches create no launch-prompt run, and their launch text is the user's.

### Wrappers and sanitizing

RimZ composes its `SystemReminder` and `PreviousAttemptFailure` wrappers through one registry, `agents::payload::RimzBlock`. Adapters hand lifecycle prompts over only as `SanitizedPrompt`. Its sanitizer unwraps user-query and paste envelopes, then peels balanced registered blocks, so run matching and recording see the authored text. A prompt that holds only a RimZ block yields no prompt. Both tags stay in `CONTROL_TAG_PREFIXES` as a backstop: an unpeelable block that keeps a control tag rejects the whole string, as does any provider-injected control tag.

### Turn output

The turn's final assistant message (`HookOutput::final_message`) becomes an `Assistant` entry when `TurnEnded` carries one. Cursor reports its response on a separate event, which `record_assistant_response` writes. On a redo, that writer skips an entry that already carries the frame's `ingress` key.

An `AwaitingInput` event with parsed questions writes an `Ask` entry: the ask id, the questions, and the agent's assistant text at ask time. Both the `Assistant` and the blocking `Ask` copy `turn_opened_by` into `reply_to` ([causality](#causality)).

A provider turn error becomes an `Error` entry when a hook decodes it (`HookOutput::turn_error`) and the merge into `AgentContext.turn_error` changes the record (`merge_turn_error_marker_and_transcript`). Replays write none. A statusline-only detection stays card enrichment, because the statusline path takes no workspace lock and writes no transcript.

`record_conversation` writes nothing for an observation that carries a provider-native parent (`AgentLifecycleObservation::parent_agent_id`), except the asks and answers of a Codex ask-queue edit. A pane-backed launched child is a full session and writes its own entries.

### What the origin guarantee covers

The guarantee covers recorded message and run origins, not raw keystrokes. `rimz pane send` carries no message record by design, and its caller is the author. One launch path falls outside it: an agent-launched peer whose adapter lacks a native turn-start or turn-end hook (`harness::run::peer_can_report`) gets no launch-prompt run, so nothing accounts for its launch text and it is recorded as a human prompt.

## Causality

Three optional field groups link entries. Each defaults empty, so older JSONL lines decode unchanged.

`message_id` stamps an entry with its matched queue record. Confirmation returns every confirmed record in the submitted batch, and each section takes the record that [section classification](#section-classification) matched to it. A headerless in-flight match also supplies `message_id` and `reply_to`, even while the record remains `Sent`. Hand-typed prompts and unmatched text carry no `message_id`.

`enqueued_at` accompanies a matched `message_id` and copies the queue record's creation time, however long delivery took. The entry's `at` stays the record time, which for a message is delivery confirmation, so bucket placement is unchanged. The reader derives `ChatLine.at` from `enqueued_at` when it is present and exposes the record time as `delivered_at`. `ChatLine.at` is the stamp: it drives the header, `--json`, header grouping, and the flip window. Order is arrival, `ChatLine::arrived_at` (`delivered_at`, else `at`), which for every log entry is the entry's own `at`. The sort and the live-cohort split key on arrival, so a line that waited sits where it landed. Nothing is backfilled from the message store, so entries written before the field keep their delivery stamp. A scheduled message is stamped with its creation time, with no floor at the scheduled moment, and sorts at its delivery. A requeue is a new record with its own creation time.

`reply_to` carries parent message ids. The loop that fills it runs through the agent context:

1. A turn start replaces `AgentContext.turn_opened_by` with every matched message id. An empty list clears the previous turn's value.
2. An agent-authored enqueue copies `turn_opened_by` into the new record's `in_reply_to`. A human sender, `--no-from`, an unnamed sender, or missing context starts a new root.
3. The turn's final `Assistant` entry and any blocking `Ask` copy `turn_opened_by` into `reply_to`. A Codex async `Ask` carries none.

A requeued message keeps `in_reply_to`, so retried text keeps its causal position.

`parent_agent_id` and `parent_agent_kind` name the direct parent of a pane-backed launched child (`stamp_parent`). The reader folds that stamp from any entry into the session identity, which keeps the child's conversation out of channel, `@all`, and parent-focused scopes after the live store row is gone. Targeting the child directly shows it.

## Reading it back

`rimz transcript` renders a channel or one agent as a threaded chat, and `rimz agents logs` renders the same view. The reader in [`cli/transcript/`](../../../crates/rimz/src/cli/transcript) applies the rules below in this order.

### Scope and the current life

The reader computes a current-life boundary: the earliest `registered_at` among the live agents in scope (`scope.rs::live_boundary`). For a channel view those are the channel's live root agents, and for an agent view they are the focused sessions, a child included. Entries that arrived before the boundary are the prior-session archive, so a message created earlier and delivered after the boundary stays in the live view. The archive is hidden by default, with a stderr hint that counts the hidden entries and names the `--all` command, and `--all` renders it under a dated marker. When every selected entry predates the boundary, or no live agent is in scope, the whole view renders as archive. The buckets are never rewritten.

### Superseded asks

`scope.rs::dedup_asks` keeps, for each agent, only its newest `Ask` and every `Ask` that an `Answer` closes by id. An unanswered ask that a newer one replaced leaves the view.

### Harness-turn hiding

The human view drops harness turns as units before assembly (`thread.rs::hide_harness_turns`). A turn's openers are the resolved `reply_to` parents of its output, or, when none were recorded, the session's latest preceding opener, which in an arrival-ordered view is the last one delivered before the output ran. An `Assistant` or `Error` entry whose openers are all harness entries hides with them. The exception is a turn that asked: when an `Ask` anywhere in the log, superseded ones included, shares one of those openers, the output stays, since that turn ends in a reply to the user's answer. `Ask` entries and agent-sent `Message` entries always stay. A kept output whose fallback opener was hidden is marked so that assembly roots it, instead of attaching it to an older visible opener.

### Thread assembly

The reader makes one pass in view order (`thread.rs::assemble_threads`). Prompts, messages, subagent reports, waits, and flips are ordered entries. Each joins its link target's thread only while that thread is current, otherwise it starts a thread at the margin, and each ordered entry makes its thread current. A message targets its latest preceding eligible `reply_to` parent: the parent is not human-authored, and its sender is the message's receiver, so a hand-off to a third agent starts a new exchange. Turn output (`Assistant`, `Ask`, `Error`) joins its latest preceding opener's thread. An `Answer` takes its ask's lane, block, and archive membership, without changing the current thread or the sender a flip attaches to. Missing or forward anchors leave entries at the margin. Two openers answered by one turn therefore stay separate, with the output under the later opener.

Threads emit in the order of their first entry, with members in view order. This keeps arrival order among ordered entries while late output sits under its opener. `--flat` skips assembly. `--last` then cuts this display order, not arrival order (`keep_last_blocks`): it keeps the last N cards, counting an ask and its folded answer once, and widens the cut back to the head of the block it lands in, so a shown thread keeps the entry that opened it.

### Answer resolution

One resolver links each answer to an earlier ask (`mod.rs::answer_asks`). An id-stamped answer links to the ask with that `AskId`, with no fallback when the id is unmatched. Only an id-less answer falls back to its agent's latest earlier unanswered ask. Resolution uses source order even after threading moves asks in display order. Placement, folding, archive selection, follow ticks, and tail counts share the resolver. A folded pair counts as one card, and an archive or tail cut keeps or drops both. A follow tick carrying an answer reprints its ask card.

### Stage lines

The reader joins the lane's `team.stage` flips (`harness::team_stage::stage_flips`) by time as flip lines. A flip targets the flipping agent's latest preceding non-answer line within the five-minute grouping window (`chat.rs::GROUP_WINDOW_SECS`). It inherits that line's lane only when the line belongs to the current thread, and otherwise opens at the margin ([loops.md § Team signals](./loops.md#team-signals)).

## Asks and answers

An agent holding a permission prompt, plan approval, or blocking question reserves its pane input: [delivery check 13](./messaging.md#the-ordered-check) holds queued text, and [model.md § Waiting and asks](../agents/model.md#waiting-and-asks) owns the `is_awaiting_input` guard that decides whether the ask holds. The ask records described here turn that reservation into structured data a script can read and answer. Codex async questions join them without reserving anything.

### Recording an ask

A blocking hook gets an `ask_` id at ingestion (`cli/hooks/lifecycle/observe.rs::record_lifecycle_observation`), written onto the `AwaitingInput` signal. The reducer projects it to `AgentState.open_ask` and clears it on the same edges that clear `waiting_since`. An event recorded without an id replays as a waiting row with no structured ask. What else is recorded depends on the ask kind:

- A question or plan hook with parsed questions appends a transcript `Ask` entry with the same id ([turn output](#turn-output)).
- A question with no parsed questions, such as a Pi extension dialog, opens `open_ask` and writes no `Ask` entry.
- A permission hook keeps a short tool summary on `open_ask` and writes no `Ask` entry. The adapter's safe options are synthesized at read time through `ask_options`.

A blocking ask closes in the transcript in one of two ways. An adapter that reports the native answer (Claude's `PostToolUse`, OpenCode's `permission_replied`) writes an id-stamped `Answer` through `record_native_answer`. Otherwise the next turn start classifies the first direct human prompt as the `Answer` ([section classification](#section-classification)), which closes the durable ask even when the provider emits no answer event. Every id-stamped answer goes through `transcript::append_answer_if_missing`, which checks and appends under one workspace lock, so a native hook racing `rimz answer` yields one record.

A Codex async question arrives as an `ask_queue` edit on an ordinary observation, not as a waiting signal. Ingestion gives each queued question an ask id, reusing the id of a listed question with the same native key, and maps each answered key to its listed id. The writer then appends one `Ask` entry per queued question, with empty text and no `reply_to`, and one id-stamped `Answer` per answered key. A replay writes no queued `Ask`. The fold keeps the list on `AgentState.queued_asks` ([model.md § Waiting and asks](../agents/model.md#waiting-and-asks)), and the Codex wire is in [adapter_codex.md § Asks and answers](../agents/adapter_codex.md#asks-and-answers).

### Reading an ask

[`AgentState::actionable_asks`](../../../crates/rimz/src/agents/state.rs) is the one list of open asks: the blocking `open_ask` while `is_awaiting_input` holds, then the queued async questions, each tagged `blocking` or `async`. `rimz asks` lists it, and `agents::read_open_ask` materializes one entry for both commands. The questions and the ask-time assistant text, exposed as `context`, join from the transcript by exact ask id only. When no `Ask` entry matches, and always for a permission ask, the materializer synthesizes one question from the ask's detail and the adapter's `ask_options`.

The materializer also computes one route (`AskRoute`). An async question routes to the pane (`AsyncPane`). So does an ask whose adapter has no answer planner, and a permission or plan ask without options (`Pane`). Every other ask is answerable from a shell. `rimz answer` refuses a pane-routed ask with exit 3 and a focus command, before it sends anything.

### Answering

`rimz answer` drives the native prompt UI through the adapter's `answer_plan`, which maps validated replies to keys, typed text, and pastes. Claude, Codex, and Pi implement it, and the trait default refuses. The key mapping for each lives on its adapter page: [Claude](../agents/adapter_claude.md#asks-and-answers), [Codex](../agents/adapter_codex.md#asks-and-answers), [Pi](../agents/adapter_pi.md#hooks-and-lifecycle). The command runs in this order:

1. Resolve the agent and its ask: an ask id selects that ask, and an address selects the agent's first actionable ask. A provider-native subagent's ask is answered through its parent's pane (`cli/mod.rs::ask_pane_owner`). Refuse a pane-routed ask, then validate every reply against the questions and build the plan. Nothing touches the pane on a failure.
2. Find the live bound pane, wake it if it is hibernated (`harness::hibernate::wake_pane`), and take its [pane write lock](./messaging.md#writing-to-the-pane).
3. Re-read the rollup and require the ask id to be among the agent's actionable asks. This compare-and-swap stops a stale answer from reaching a newer prompt (exit 2, "was answered or replaced before any key was sent").
4. Send the plan's steps, paced like message segments (`message::send::Pacer`).
5. Poll every 100 ms, up to 30 seconds by default (`--wait`), until the ask leaves the actionable list or a transcript `Answer` with that id appears. On timeout the command exits 4 and says not to resend.
6. Append the structured `Answer` entry when the transcript does not already hold one for that id.

`--no-wait` skips steps 5 and 6, so the command never records an answer the agent did not acknowledge. The full exit-code table is in [cli/asks.md § Exit codes](../../reference/cli/asks.md#exit-codes).

## See also

- [messaging.md](./messaging.md): the delivery pipeline that writes the confirmed prompts this log records.
- [model.md § Waiting and asks](../agents/model.md#waiting-and-asks): when an agent counts as waiting, and the async question queue.
- [attribution.md](../agents/attribution.md): the PR footnote that counts human-origin prompts across the whole log.
- [cli/transcript.md](../../reference/cli/transcript.md) and [cli/asks.md](../../reference/cli/asks.md): the user-facing flags.

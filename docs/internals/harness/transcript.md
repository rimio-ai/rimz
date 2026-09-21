# The transcript and asks

> How RimZ records a conversation and reads it back: the durable transcript log, its entry kinds, the causal links between entries, the `rimz transcript` projection, and the ask records that turn a blocked agent's question into structured data. The log is [`transcript.rs`](../../../crates/rimz/src/transcript.rs); the hook-side writer is [`cli/hooks/lifecycle/transcript.rs`](../../../crates/rimz/src/cli/hooks/lifecycle/transcript.rs), the reader is [`cli/transcript/`](../../../crates/rimz/src/cli/transcript), and the ask commands are [`cli/asks.rs`](../../../crates/rimz/src/cli/asks.rs) and [`cli/answer.rs`](../../../crates/rimz/src/cli/answer.rs). How text gets into a pane in the first place is [messaging.md](./messaging.md). For users, the commands are [cli/transcript.md](../../reference/cli/transcript.md) and [cli/asks.md](../../reference/cli/asks.md).

## The log

The transcript is a RimZ-owned conversation log, separate from each provider's native session files. Ended agents, past channels, and their asks and answers stay readable after the native files rotate away.

Hook and delivery paths append entries to fixed 7-day buckets (`FILE_DAYS`) at `transcript/<bucket-start>.jsonl` in the workspace store, under the workspace lock. The buckets are append-only and never pruned. Readers sort by recorded timestamp, so a bucket boundary carries no ordering meaning.

Every transcript reader skips legacy paste fragments written by RimZ 0.4.3 and earlier: a `Prompt` with no `from` or `message_id` whose entire trimmed text is one `<pasted_content id="X">` or `</pasted_content id="X">` tag line. These are provider wrappers, not human prompts; genuine prompts containing a wrapper pair mid-text stay. The append-only files are not rewritten.

The test is unconditional, not version-scoped: it applies to every entry read, including ones written after the adapter learned to peel the envelope. A prompt whose whole text is a single tag line is therefore indistinguishable from a fragment and is dropped as well, which in practice means only a human pasting the tag itself while discussing this behaviour. Matching a whole entry exactly is the narrowest form that is safe against text RimZ never normalized, and that residue is its accepted cost.

Three nearby reads use other sources. Supervised-run streaming tails the provider-native transcript through the adapter-owned source ([scripting.md § Output and input projections](./scripting.md#output-and-input-projections)); the context and spend gauges read those same native stores ([model.md § Enrichment](../agents/model.md#enrichment)); and the message audit trail in the event log carries no text ([messaging.md § Storage and audit](./messaging.md#storage-and-audit)).

## Entry kinds

`TranscriptKind` has eight variants.

| Kind | Records | Human rendering |
| --- | --- | --- |
| `Prompt` | A human prompt when no question is open, or a confirmed system prompt with `from: "rimz"` | `user: @receiver, text` for a human prompt; a system prompt is hidden with the output of the turn it opens |
| `Message` | An inter-agent delivery, or a launched child's launch brief, with structured `from` | `@sender: @receiver, text` |
| `SubagentReport` | The status-only launched-child fleet digest, with `from: rimz` | Hidden with the output of the turn it opens |
| `Wait` | A `Type: WAIT`, `Type: SIGNAL`, or `Type: STAGE` delivery, with `from: rimz` | Hidden with the output of the turn it opens |
| `Assistant` | A root turn's final assistant message | `@receiver: text` |
| `Ask` | A native question or plan approval, carrying option labels and descriptions | The agent's question |
| `Answer` | The effective answer, from the native prompt UI or the first human prompt submitted while a question is open | `you` to the agent, folded into its ask card |
| `Error` | A hook-path provider error newly merged into `AgentContext.turn_error` | `@receiver: error text`, styled as an error |

`rimz transcript --json` includes every hidden entry. [`TranscriptEntry::is_harness()`](../../../crates/rimz/src/transcript.rs) is the one predicate for harness entries (`SubagentReport`, `Wait`, and a `Prompt` whose `from` is `HARNESS_FROM`), shared by the human renderer and conversation counts.

## Writing entries

The receiver's turn-start hook writes the entries for a delivered prompt. It parses the [message header](./messaging.md#the-message-header) once per section and maps the `Type`:

| Header `Type` | Entry |
| --- | --- |
| `AGENT_MESSAGE` | `Message`, with structured `from` |
| `SUBAGENT_REPORT` | `SubagentReport`, with `from: rimz` |
| `WAIT`, `SIGNAL`, `STAGE` | `Wait`, with `from: rimz` |
| `USER_MESSAGE` | `Prompt`, header removed, no `from` |
| none, confirmed system record | `Prompt` with `from: rimz` |
| none, unconfirmed | `Prompt` (human direct input) |

A batched delivery splits on each blank line that introduces another header of those six types, so every section becomes its own entry.

Three cases change the mapping:

- While the agent has an open question, the first direct human `Prompt` segment becomes that ask's id-stamped `Answer`. An attributed queue record never answers a question.
- A launched child's initial headerless prompt becomes a `Message` from its parent when it exactly matches the durable subagent run prompt. Later headerless input stays a human `Prompt`.
- Text around a confirmed headered batch becomes separate direct-input `Prompt` entries ([messaging.md § Confirmation and retry](./messaging.md#confirmation-and-retry)).

The turn's final assistant message becomes an `Assistant` entry. A provider turn error becomes an `Error` entry only on the hook-path merge (`StopFailure` or a `Stop` tail refresh). A statusline-only detection stays card enrichment, because the statusline path takes no lock and writes no transcript.

## Causality

Three optional field groups link entries. Each defaults empty, so older JSONL lines decode unchanged.

`message_id` stamps a delivered entry with its queue record. Confirmation returns every record in the submitted batch; alignment restores each section from the durable record boundaries, and each section then matches a returned record by exact body text, in order. Hand-typed prompts and unmatched text carry no `message_id`.

`enqueued_at` accompanies a matched `message_id`, copying the queue record's creation time however long delivery took. The log's `at` stays the record time, which for a message is delivery confirmation, so bucket placement and reader order are unchanged. The view derives `ChatLine.at` from `enqueued_at` when it is there and exposes the record time as `delivered_at`. `ChatLine.at` is the stamp: it drives the header, `--json`, header grouping, and the flip window. Order is arrival, `ChatLine::arrived_at` (`delivered_at` else `at`), which for every log entry is the log's own `at`; the sort, the live-cohort split, and the `--last` cut key on it, so a line that waited sits where it landed. Nothing is backfilled from the message store, so entries written before the field keep their delivery stamp. A scheduled message is stamped from its creation time, with no floor at the scheduled moment, and reads at its delivery; a requeue is a new record with its own.

`reply_to` carries parent message ids. The loop that fills it runs through the agent context:

1. A turn-start replaces `AgentContext.turn_opened_by` with every matched message id. An empty vector clears the previous turn's value.
2. An agent-authored enqueue copies `turn_opened_by` into the new record's `in_reply_to`. A human sender, `--no-from`, an unnamed sender, or missing context starts a new root.
3. The turn's final `Assistant` entry and any mid-turn `Ask` copy `turn_opened_by` into `reply_to`.

A requeued message keeps `in_reply_to`, so retried text keeps its causal position.

`parent_agent_id` and `parent_agent_kind` name the direct parent of a pane-backed launched child. The reader folds that stamp from any entry into the session identity, which keeps the child's conversation out of channel, `@all`, and parent-focused scopes after the live store row is gone. Targeting the child directly still shows it.

## Reading it back

`rimz transcript` renders a channel or one agent as a threaded chat. The reader in [`cli/transcript/`](../../../crates/rimz/src/cli/transcript) applies four rules.

**Scope and current life.** The reader computes a current-life boundary: the earliest `registered_at` among the matching live root agents. Entries that arrived before it are the prior-session archive, so a message created earlier and delivered after the boundary stays in the live view. With a live cohort the archive is hidden by default and `--all` renders it under a dated marker; with no live cohort the whole scope renders as archive. The buckets are never rewritten.

**Harness-turn hiding.** The human view drops harness turns as units before assembly. A turn's openers are the resolved `reply_to` parents of its output, or the session's latest preceding opener when none were recorded, which in an arrival-ordered view is the last one delivered before the output ran. An `Assistant` or `Error` entry whose openers are all harness entries hides with them, unless an `Ask` anywhere in the log (superseded ones included) shares one of those openers, since that turn ends in a reply to the user's answer. `Ask` entries and agent-sent `Message` entries always stay. A kept output whose fallback opener was hidden is marked so assembly roots it, instead of attaching it to an older visible opener.

**Thread assembly.** The reader makes one pass in view order. Prompts, messages, subagent reports, waits, and flips are ordered entries: each joins its link target's thread only if that thread is still current, otherwise it starts a thread at the margin. Each ordered entry makes its thread current. A message targets its latest preceding eligible `reply_to` parent, whose sender must be the message's receiver; a hand-off to a third agent starts a new exchange. Turn output (`Assistant`, `Ask`, `Error`) joins its latest preceding opener's thread, and an `Answer` joins its latest preceding resolved parent, without changing the current thread. Missing or forward anchors leave entries at the margin. Two openers answered by one turn therefore stay separate, with output under the later opener. Threads emit by their first entry's view index, with members in view order, preserving arrival order among ordered entries while allowing late output to sit under its opener. `--flat` skips assembly.

**Stage lines.** The reader joins the lane's `team.stage` signals by time as flip lines. A flip targets the flipping agent's latest preceding line within the grouping window and inherits its lane only when that line belongs to the current thread; otherwise it opens at the margin ([loops.md § Team signals](./loops.md#team-signals)).

## Asks and answers

An agent holding a permission prompt, plan approval, or question reserves its pane input: [delivery check 9](./messaging.md#the-ordered-check) holds queued text, and [model.md § Waiting and asks](../agents/model.md#waiting-and-asks) owns the `is_awaiting_input` guard that decides whether the ask still holds. The ask records described here turn that reservation into structured data a script can read and answer.

### Recording an ask

A blocking hook mints an `ask_` id at ingestion ([`observe.rs`](../../../crates/rimz/src/cli/hooks/lifecycle/observe.rs)) and writes it onto the `AwaitingInput` signal. The reducer projects it to `AgentState.open_ask` and clears it on the same edges that clear `waiting_since`. An event recorded without an id replays as a waiting row with no structured ask.

What else is recorded depends on the ask kind:

- Question and plan hooks append a transcript `Ask` entry with the same id, the parsed questions, and the agent's assistant text at ask time.
- Permission hooks keep a short tool summary on `open_ask` and append no transcript entry, because no native event closes a permission ask with an answer. The adapter's safe options are synthesized at read time through `ask_options` ([`agents/open_ask.rs`](../../../crates/rimz/src/agents/open_ask.rs)).

A later turn-start hook classifies the first human prompt as an id-stamped free-text `Answer` while that question is open, which closes the durable ask even when the provider emits no native answer event.

`rimz asks` treats `is_awaiting_input` plus `open_ask` as truth. It joins the parsed questions and assistant text from the transcript by ask id only, and exposes that text as `context`.

### Answering

`rimz answer` drives the native prompt UI through the adapter's `answer_plan`, which maps validated answers to keys, typed text, and pastes. Claude, Codex, and Pi implement it; the trait default refuses, and `rimz answer` exits 3 with "does not support structured answers". The command runs in this order:

1. Resolve the agent and its open ask, then validate every reply against the questions and build the plan. Nothing touches the pane on a validation failure.
2. Find the live bound pane and take its [pane write lock](./messaging.md#writing-to-the-pane).
3. Re-read the rollup and require the ask id to still be the agent's current open ask. This compare-and-swap stops a stale response from answering a newer prompt (exit 2, "no longer current").
4. Send the plan's steps, paced like message segments.
5. Poll every 100 ms, up to 30 seconds by default (`--wait`), until the ask leaves the rollup or a transcript `Answer` with that id appears. On timeout it exits 4.
6. Append the structured `Answer` entry when the transcript does not already hold one for that id.

`--no-wait` skips steps 5 and 6, so the command never records an answer the agent did not acknowledge.

For Claude, a user-question answer maps to native keys and paste actions, permission `allow` to typing `1`, and plan `approve` to Shift-Tab. A permission or plan ask accepts only that one confirmable action; deny, persistent grants, refinement text, and manual-review approval fail before delivery with an error that names the pane as the place to do them.

## See also

- [messaging.md](./messaging.md): the delivery pipeline that writes the confirmed prompts this log records.
- [model.md § Waiting and asks](../agents/model.md#waiting-and-asks): when an agent counts as waiting.
- [cli/transcript.md](../../reference/cli/transcript.md) and [cli/asks.md](../../reference/cli/asks.md): the user-facing flags.

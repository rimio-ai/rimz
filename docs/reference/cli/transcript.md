# Transcript CLI

`rimz transcript` reads RimZ's durable transcript log and renders a channel or one agent as a timestamped chat log. It reads only — nothing is sent, nothing changes — and it reaches back past what the agents keep: because RimZ records every prompt, answer, and inter-agent message itself, the log survives ended agents whose native transcript files have rotated away. It targets agents and channels with the [agent-address grammar](./agents.md#addressing-agents). Reading the conversation back as an everyday move is the [messaging guide](../../guide/messaging.md); the log model (entry kinds, JSONL buckets, retention) is [message.md → Transcript](../../internals/harness/messaging.md#transcript).

```sh
rimz transcript @swift-otter            # one agent's channel messages
rimz transcript @codex#cli-docs --last 4
rimz transcript run_0123456789abcdef0123456789abcdef
rimz transcript #cli-docs               # the channel chat log
rimz transcript @all#cli-docs --last 12
rimz transcript --all                   # include the dated history archive
rimz transcript --flat                  # restore pure timestamp order
rimz transcript --json
```

A channel target (`#worktree`, `@all`, or no target for the current channel) projects every root agent's transcript-log entry in that exact lane into one chat log. The default view starts at the current live cohort for that scope, so a same-name team or worktree relaunch opens on its living conversation instead of replaying the prior one. Older lines remain in the append-only log, `--all` shows them under a dated history archive, and an empty scope exits successfully with a short note.

A single-agent target is deliberately non-ambiguous: a supervised-run id resolves to that run's bound root agent session, an exact session id wins across channels, and otherwise a handle picks a live session in the current room when one exists, then the latest transcript activity. The single-agent view builds the same channel log and filters it to messages the focal agent sent or received, so sent messages appear from peers' logs as well as received messages from the focal log. Sends made with `--no-from` look like human prompts, and cross-channel sends involving agents outside the focal channel are outside this view.

Rendering: a message that opens an exchange stays at its timestamp in the margin, and every causally linked reply expands chronologically beneath it behind a `│` spine. Threads are one level deep; a later hand-off in the same exchange remains a thread member rather than nesting another level. Unlinked history and a reply whose parent is outside the selected scope stay at the margin. `--flat` restores pure timestamp order.

Headers put the sender first, add the receiver with `→` when one exists, and show `HH:MM`; a thread reply on another day also shows its date. Consecutive messages from the same sender-to-receiver pair group under one header only within the same margin or thread lane. Message bodies highlight `@agent` and `#channel` mentions, and provider API error entries render as error-styled agent lines. Blocking asks render as cards with a left spine, option lists, each option's description under its label, folded answers, and `◌ unanswered` when no answer exists in the log. Peer-opened turns include the receiver's assistant reply.

`--last <N>` keeps the last N lines after thread blocks are expanded into display order, so the tail follows the rendered room rather than the raw timestamp sequence. `--json` emits `{channel, focus, entries}` for both channel and agent targets, with `archived_count` when prior-session lines are hidden. JSON entries remain timestamp-ordered and carry optional `message_id` plus `reply_to: [message_id, ...]`, so clients can rebuild the same thread components; absent fields mean the entry has no recorded linkage.

For an agent-centric tail that follows new lines as they land, [`rimz agents logs <ref>`](./agents.md#logs) uses this same transcript scope and rendering.

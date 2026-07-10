# Amp adapter

> The agent-agnostic boundary and state machine are in [model.md](./model.md), and the account model is in [providers.md](./providers.md). The verified upstream Plugin API and CLI surface is in [amp-reference.md](../../externals/agent-adapter/amp-reference.md).

Amp exposes no command-hook, statusline, or interactive RPC protocol, so RimZ owns the wire: [`plugin.ts`](../../../crates/rimz/src/agents/amp/plugin.ts) is installed at `~/.config/amp/plugins/rimz.ts` and forwards observation-only envelopes to `rimz hooks feed --source amp`. Upstream drift in that envelope is a RimZ adapter bug.

## Hooks and lifecycle

| Native event or state | Internal signal | Normalized fields |
| --- | --- | --- |
| `session.start` | `Registered` | thread id, cwd, mode/model, effort |
| `agent.start` | `TurnStarted` | sanitized user prompt |
| `tool.result` | `ToolUsed` | tool name, completion status, file-edit proof |
| `agent.end` | `TurnEnded` | done versus error/cancelled |
| thread state enters `awaiting-approval` | `AwaitingInput { Permission }` | no ask detail or external answer handle |

Amp can run several threads concurrently in one CLI. The plugin forwards an event only when its thread matches `amp.activeThread.current`; execute and runner modes report with a null active thread and therefore forward normally. Switching the focused thread causes Amp to fire `session.start` for that thread. RimZ stamps that registration as `Fresh` pane occupancy, including when returning to an existing conversation, so the same-pane supersession rules retire the previously focused row and revive the newly focused one.

The permission path observes Amp's own UI without joining its decision chain. `awaiting-approval` carries neither tool detail nor a resolver, so the row reads waiting and the user answers in Amp; `rimz answer` remains unsupported. The plugin deliberately does not register `tool.call`, whose cancellation and multi-plugin result composition are not documented.

Amp has no session-end, notification, compaction, or interactive subagent events. Pane liveness plus the rollup reaper derive session removal, and turn-end plus permission waiting plus the stall window cover the attention-bearing portion of idle. Automatic compaction has no manual command or lifecycle bracket.

## Context and transcript

The plugin carries the current built-in mode, or a custom agent's explicit model, plus reasoning effort from `thread.agent()`. Amp's plugin transcript messages carry no token usage, cost, timestamps, or stable local path, so context usage, rich context, transcript-tail enrichment, and interactive subagent identity are unsupported.

Completed tools are always proof of work. `amp.helpers.filesModifiedByToolCall` is the primary edit signal and covers Amp's edit tools plus in-place shell edits; the descriptor's static tool table is the fallback for older or sparse envelopes.

## Account and balance

The account probe checks only whether `~/.local/share/amp/secrets.json` is a credential file. It never reads or parses the secret. A present file reports a minimal pay-per-use Amp account without rate-limit windows, a missing file reports logged out, and an inaccessible or malformed path reports unavailable.

## Cost

Amp exposes `amp usage` and thread usage as human-readable text with no documented machine schema. Realtime cost, historical spend, included-balance windows, and OAuth usage are unsupported until Amp publishes a stable structured surface.

## Launch and install

`rimz agents amp` launches the stock interactive CLI. Supervised prompts use `amp -x <prompt> --plugin-ready-timeout 30` so execute mode waits for plugin handlers before starting; profiles map `model` to Amp's `--mode` dial and `effort` to `--effort`. `amp threads continue <T-id>` resumes a thread, while fork and manual compaction have no native command.

Amp's default permission posture runs without approval. RimZ passes no permission-mode flag because Amp exposes none for native ask or plan postures; user settings and plugins continue to own any approval UI.

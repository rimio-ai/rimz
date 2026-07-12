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

The plugin forwards registration before its asynchronous agent-definition and thread-state reads, then serializes helper subprocesses through one bounded queue. A fast first turn therefore cannot overtake registration in the store, and a slow metadata read or focus switch cannot discard the registration boundary. Later envelopes carry mode/model and effort once the enrichment read resolves.

The permission path observes Amp's own UI without joining its decision chain. `awaiting-approval` carries neither tool detail nor a resolver, so the row reads waiting and the user answers in Amp; `rimz answer` remains unsupported. The plugin deliberately does not register `tool.call`, whose cancellation and multi-plugin result composition are not documented.

`agent.end` carries a three-way `status`: `done` folds to a clean `TurnEnded`, while both `error` and `cancelled` fold to `TurnEnded { errored: true }` and settle the row to `failed`. Amp is unusual in reporting user cancellation as an explicit terminal status rather than a derived transcript sentinel (Claude) or rollout marker (Codex), so it is the one built-in that could distinguish an Esc-cancelled turn (settle to `idle`, the [turn-interruption](./model.md#displayed-status) rung) from a real error without any transcript archaeology. RimZ has no hook-path seam for that today — `observe_turn_interrupted` is wired only through Claude's statusline transport — so the current adapter folds `cancelled` into `failed` to match Pi's aborted-rides-errored precedent. See [Deferred edges](#deferred-edges) for the abstraction note.

Amp has no session-end, notification, compaction, or interactive subagent events. Pane liveness plus the rollup reaper derive session removal, and turn-end plus permission waiting plus the stall window cover the attention-bearing portion of idle. Automatic compaction has no manual command or lifecycle bracket.

## Context and transcript

The plugin carries the current built-in mode, or a custom agent's explicit model, plus reasoning effort from `thread.agent()`. Amp's plugin transcript messages carry no token usage, cost, timestamps, or stable local path, so context usage, rich context, transcript-tail enrichment, and interactive subagent identity are unsupported.

Completed tools are always proof of work. `amp.helpers.filesModifiedByToolCall` is the primary edit signal and covers Amp's edit tools plus in-place shell edits; the descriptor's static tool table is the fallback for older or sparse envelopes.

`agent.end.messages` supplies the final non-empty assistant text block. The plugin stamps that text onto the end envelope, and the adapter normalizes it into the transcript and supervised-run result, so `rimz agents amp -p` returns the answer through the same run-store path as the other built-ins.

## Account and balance

The account probe checks whether `AMP_API_KEY` is non-empty or `~/.local/share/amp/secrets.json` is a credential file. It never reads or parses the secret. Either credential source reports a minimal pay-per-use Amp account without rate-limit windows, a missing file reports logged out when `$HOME` is available, and an inaccessible path or unavailable home reports unavailable.

## Cost

Amp exposes `amp usage` and thread usage as human-readable text with no documented machine schema. Realtime cost, historical spend, included-balance windows, and OAuth usage are unsupported until Amp publishes a stable structured surface.

## Launch and install

`rimz agents amp` launches the stock interactive CLI. Supervised prompts use `amp -x <prompt> --plugin-ready-timeout 30` so execute mode waits for plugin handlers before starting; profiles map `model` to Amp's `--mode` dial and `effort` to `--effort`. The supervised result rides the plugin's `agent.end.messages` slice, not stdout parsing, so `-p` returns through the same run-store path as the other built-ins. `amp threads continue <T-id>` resumes a thread, while fork and manual compaction have no native command.

Profiles pass a configured `--mode` verbatim. The refresh binary's `amp --help` advertises only `low, medium, high`, but `amp plugins show-docs` types `ultra` as a valid `BuiltinAgentMode`, so RimZ does not validate the value against the narrower help line ([amp-reference.md → Models, modes, and effort](../../externals/agent-adapter/amp-reference.md#models-modes-and-effort)).

Amp's default permission posture runs without approval. RimZ passes no permission-mode flag because Amp exposes none for native ask or plan postures; user settings and plugins continue to own any approval UI.

## Implemented now

The landed adapter wires the full turn lifecycle (`session.start` → registered, `agent.start` → turn start, `tool.result` → proof-of-work with `filesModifiedByToolCall` driving the `reasoning → acting` edge, `agent.end` → turn end), permission waiting off `PluginThread.state == "awaiting-approval"`, mode/model and effort enrichment from `thread.agent()`, the supervised-run final answer from `agent.end.messages`, the whole-file plugin install/preview/uninstall, thread resume via `amp threads continue`, and a secret-safe account presence probe. Session end derives from pane liveness plus the rollup reaper; focus switches restamp `Fresh` occupancy so same-pane supersession retires the previously focused thread and revives the newly focused one.

## Deferred edges

Every item below is a signal Amp does not yet expose in a stable machine-readable form, or a RimZ abstraction that would need a new seam to carry one that it does. They are ordered roughly by user-visible value.

- **`cancelled` should settle to idle, not fail.** Amp's `agent.end` reports Esc-cancellation as an explicit `status: "cancelled"` — a cleaner signal than Claude's transcript sentinel or Codex's rollout marker — yet the adapter currently folds it into `failed` and nags with a `!`. Settling it to the [turn-interruption](./model.md#displayed-status) rung (`idle`, at rest, no result) would need a hook-path turn-interruption seam: a trait method symmetric to `observe_turn_error_from_hook` (say `observe_turn_interrupted_from_hook`) wired into [`merge_agent_context_sidecars`](../../../crates/rimz/src/cli/hooks/lifecycle/context.rs) to stamp `AgentContext::turn_interrupted`. Today `observe_turn_interrupted` reaches the sidecar only through Claude's statusline transport, never the shared hook path. This is a RimZ-abstraction change deferred for future consideration.
- **Supervised runs discard token and cost data.** `amp -x --stream-json` is the one Amp transport that carries per-response `usage` and `max_tokens` ([amp-reference.md → Supervised runs](../../externals/agent-adapter/amp-reference.md#supervised-runs-and-stream-json)). RimZ takes the final answer from the plugin wire and ignores stdout, so a `-p` run records no context gauge or `live$`. Wiring stream-JSON parsing into the supervised-run path would add context and realtime-cost coverage for the `-p` slice only (never projected onto unrelated interactive threads), at the cost of a scripting-contract change.
- **Supervised-thread resumability vs. archive clutter.** `amp -x` archives new execute-mode threads by default. RimZ keeps the default so automated loops do not flood `amp threads list`, but has not verified that `amp threads continue <T-id>` can still resume an archived thread; if it cannot, `resume_command` needs `--no-archive-after-execute` on the launch argv. Verify on a live account before changing the posture — the trade-off is real in both directions.
- **The plugin keeps unbounded per-thread state.** `plugin.ts` tracks `gauges`, `states`, `subscribedThreads`, and `registeredThreads` keyed by thread id, and Amp fires no `session.end`, so a long-lived `amp --no-tui` runner accumulates entries for every thread it ever hosts. The maps are tiny, but a future cleanup would need either a thread-closed event Amp does not publish or a periodic prune against `amp.threads`.
- Permission coverage is conditional on another Amp policy surface opening `awaiting-approval`. RimZ maps that native state end to end and therefore declares the concern wired, while the current coverage abstraction cannot also express that the default Amp posture creates no prompt and that legacy/custom approval dialogs still need live verification.
- Permission presets currently render only provider argv. Amp needs an installed policy handler for `ask` and prompt-level guidance for `plan`, so representing those postures faithfully would require an adapter-owned prompt transform or conditional managed-plugin policy rather than a fake CLI flag.
- The plugin process stamps `process.pid` as the agent PID because Amp loads the system plugin in the CLI runtime. Verify that identity with two logged-in Amp CLIs in the same cwd; if Amp moves plugins into workers, the adapter needs an upstream host-PID field or a RimZ instance-binding seam rather than cwd inference.
- Interactive context usage, per-thread spend, built-in subagent identity, permission details and resolution, automatic-compaction brackets, and remote-control readiness remain deferred until Amp exposes stable machine-readable signals.
- Amp publishes a paginated in-process transcript API. RimZ currently consumes the bounded `agent.end.messages` slice needed for final answers; a future rich-context transport can page `PluginThread.messages({ full: true })` without putting provider payloads below the adapter boundary.

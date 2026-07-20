# RimZ internals

How RimZ works under the hood, one subsystem per document. These pages are for contributors who read the code; they point into `crates/rimz/src/` rather than paraphrasing it. If you are here to use RimZ, start with the [user documentation](../README.md) instead.

Read the shape first: [DESIGN.md](../../DESIGN.md) states the attention problem, the design pillars, and the invariants; [ARCHITECTURE.md](../../ARCHITECTURE.md) is the runtime shape and the on-disk state. Each subsystem below carries its own `AGENTS.md` contract in the matching source tree.

## The agent layer

`agents/` models a running agent and prices it.

| Page | What it owns |
| --- | --- |
| [model.md](./agents/model.md) | The agent model: the rollup, the state machine, the adapter boundary, and the live-context read path. |
| [plugin.md](./agents/plugin.md) | Third-party plugin loading, the canonical process wire, derived descriptors, and probe execution. |
| [claude.md](./agents/claude.md), [codex.md](./agents/codex.md), [amp.md](./agents/amp.md), [copilot.md](./agents/copilot.md), [kimi.md](./agents/kimi.md), [pi.md](./agents/pi.md), [opencode.md](./agents/opencode.md), [antigravity.md](./agents/antigravity.md), [cursor.md](./agents/cursor.md), [droid.md](./agents/droid.md), [kiro.md](./agents/kiro.md), [qwen.md](./agents/qwen.md), [grok.md](./agents/grok.md) | Per-kind adapter mappings: how each native event, transcript, and account surface folds onto RimZ's types. |
| [providers.md](./agents/providers.md) | Accounts, balances, spend, and the token-pricing table behind the provider dashboard. |

## The harness

`harness/` runs the fleet: spawn, address, message, and reclaim.

| Page | What it owns |
| --- | --- |
| [harness.md](./harness/harness.md) | Layout IR, the address grammar, supervised `-p` runs and the run wake, and the scheduled loop tasks. |
| [messaging.md](./harness/messaging.md) | Message routing: send modes, durable records, the delivery pipeline, reply waits, the channel lanes, the transcript, and the ask lifecycle. |
| [worktrees.md](./harness/worktrees.md) | RimZ-owned Git worktrees: creation, the ownership marker, seeding, and landed-work cleanup. |
| [trust.md](./harness/trust.md) | The permission model: the executable launch surface, grants, and the stale-grant diff. |

## The sidebar

`sidebar/` renders presence and routes attention.

| Page | What it owns |
| --- | --- |
| [sidebar.md](./sidebar/sidebar.md) | Rendering mechanics: presence, ranking, layout, and recovery. |
| [state.md](./sidebar/state.md) | The data plane: the producer/consumer split, the published caches, push channels, fusion, and timing. |
| [notifications.md](./sidebar/notifications.md) | Best-effort attention alerts over the same state. |
| [pets.md](./sidebar/pets.md) | Renderer-local attention art for the provider dashboard. |

## Single-doc subsystems

Each of these subsystems is one file at the top level.

| Page | What it owns |
| --- | --- |
| [theme.md](./theme.md) | The shared semantic palette, provider identity, state vocabulary, and human value formats. |
| [store.md](./store.md) | The durable state engine: the on-disk shape, the write classes, the event log, and wakeups. |
| [multiplexers.md](./multiplexers.md) | The Zellij and tmux contracts behind the backend seam. |
| [remote.md](./remote.md) | SSH attach, the reconnect policy, and link health. |
| [web.md](./web.md) | Zellij and ttyd browser access. |
| [welcome.md](./welcome.md) | The lobby room picker and `rimz stats`. |
| [diagnostics.md](./diagnostics.md) | The diagnostics log, the frame observer, and off-box Sentry. |
| [performance.md](./performance.md) | The performance model: threads and clocks, the cost map, the principles, and fleet overhead. |
| [profiling.md](./profiling.md) | The field guide for profiling a live fleet. |

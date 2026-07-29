# RimZ internals

How RimZ works under the hood, one subsystem per document. These pages are for contributors who read the code; they point into `crates/rimz/src/` rather than paraphrasing it. If you are here to use RimZ, start with the [user documentation](../README.md) instead.

Read the shape first: [DESIGN.md](../../DESIGN.md) states the attention problem, the design pillars, and the invariants; [ARCHITECTURE.md](../../ARCHITECTURE.md) is the runtime shape and the on-disk state. Each subsystem below carries its own `AGENTS.md` contract in the matching source tree.

## The agent layer

`agents/` models a running agent and prices it.

| Page | What it owns |
| --- | --- |
| [model.md](./agents/model.md) | The agent model: the rollup, the state machine, the displayed-status ladder, and the instance lifecycle. |
| [adapter.md](./agents/adapter.md) | The adapter layer: the registry, the capability traits, the hook path, install, context sources, and declared coverage. |
| [plugin.md](./agents/plugin.md) | Third-party plugin loading, the canonical process wire, derived descriptors, and probe execution. |
| [adapter_claude.md](./agents/adapter_claude.md), [adapter_codex.md](./agents/adapter_codex.md), [adapter_amp.md](./agents/adapter_amp.md), [adapter_copilot.md](./agents/adapter_copilot.md), [adapter_kimi.md](./agents/adapter_kimi.md), [adapter_pi.md](./agents/adapter_pi.md), [adapter_opencode.md](./agents/adapter_opencode.md), [adapter_antigravity.md](./agents/adapter_antigravity.md), [adapter_cursor.md](./agents/adapter_cursor.md), [adapter_droid.md](./agents/adapter_droid.md), [adapter_kiro.md](./agents/adapter_kiro.md), [adapter_qwen.md](./agents/adapter_qwen.md), [adapter_grok.md](./agents/adapter_grok.md) | Per-kind adapter mappings: how each native event, transcript, and account surface folds onto RimZ's types. |
| [providers.md](./agents/providers.md) | Accounts, balances, spend, and the token-pricing table behind the provider dashboard. |

## The harness

The harness runs the fleet: spawn, address, message, and reclaim. It is a product area rather than a single module, spanning `harness/`, `message/`, `worktree.rs`, and `trust.rs`. Start at [fleet.md](./harness/fleet.md), which maps the area and names the source tree behind each page.

| Page | What it owns |
| --- | --- |
| [fleet.md](./harness/fleet.md) | The area map and the launch core: the rules that shape the design, the state-machine index, the layout IR, the exec wrapper, the address grammar, resume planning, and pane reclamation. |
| [scripting.md](./harness/scripting.md) | Supervised `-p` runs: the durable run record, the completion fold, the wake socket, verification and retry, and the output projections. |
| [subagents.md](./harness/subagents.md) | Agent-launched children: the agent-only doorway, the ancestry stamp and its depth cap, caller-scoped verbs, and the boundary with provider-native subagents. |
| [loops.md](./harness/loops.md) | Loop scheduling: the task catalog and its sources, elder firing, the fire gate ladder, run history, and the assist log. |
| [budget.md](./harness/budget.md) | Dollar caps: the scopes, the ledgers on disk, the verdict, the human waiver, the pane interrupt, and the fail-fast gate. |
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
| [pets.md](./sidebar/pets.md) | The dashboard pet: action projection, animation tracks, asset loading, and the pixel and cell-art render tiers. |

## Single-doc subsystems

Each of these subsystems is one file at the top level.

| Page | What it owns |
| --- | --- |
| [theme.md](./theme.md) | The four-layer color pipeline, the glyph catalog, provider identity, and the shared human value formats. |
| [store.md](./store.md) | The durable state engine: the on-disk shape, the write classes, the event log, and wakeups. |
| [multiplexers.md](./multiplexers.md) | The `MuxBackend` seam: backend selection, pane and view identity, presence and focus, sidebar repair, session lifecycle, the Zellij and tmux backends, and the Zellij presence plugin. |
| [rimzd.md](./rimzd.md) | The managed `rimzd` view: its panes, how they are identified, and how they are repaired. |
| [remote.md](./remote.md) | SSH attach and aliases, the reconnect supervisor, link health, port forwarding, and bandwidth attribution. |
| [web.md](./web.md) | Shared ttyd browser access for Zellij and tmux rooms. |
| [stats.md](./stats.md) | The `rimz stats` panel: the spend cache it reads, the window model, the render ladder, and the held dashboard. |
| [diagnostics.md](./diagnostics.md) | The diagnostics log, the frame observer, and off-box Sentry. |
| [performance.md](./performance.md) | The performance model: threads and clocks, the cost map, the principles, and fleet overhead. |
| [profiling.md](./profiling.md) | The field guide for profiling a live fleet. |

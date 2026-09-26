# RimZ internals

These pages document how RimZ works, one subsystem at a time, for contributors who read the code. Each page points into `crates/rimz/src/` by path and symbol, and each module's `//!` header stays the authority for its own file. To use RimZ rather than change it, start at the [user documentation](../README.md).

Read the shape before a subsystem. [DESIGN.md](../../DESIGN.md) states the attention problem, the design pillars, and the invariants; [ARCHITECTURE.md](../../ARCHITECTURE.md) gives the runtime shape and the on-disk state. When you start from a source tree instead of a topic, [from a source tree to its page](#from-a-source-tree-to-its-page) names the page that documents it.

## The agent layer

`agents/` turns each coding agent's native events into one provider-neutral model, and prices and accounts for what the agent spends.

| Page | What it owns |
| --- | --- |
| [model.md](./agents/model.md) | The agent model: status and phase, the rollup and its field lifetimes, the state machine, publishing transitions, the displayed status, activity clocks, and enrichment. |
| [instances.md](./agents/instances.md) | Agent instances: recognizing a hosted CLI, binding a session to a pane, same-pane ownership, launch identity across conversations, instance exit, and session death. |
| [attribution.md](./agents/attribution.md) | Effort attribution: record selection by lane and lifetime, the seat fold, where each figure comes from, and membership. |
| [adapter.md](./agents/adapter.md) | The adapter layer: the registry, the spec and the capability traits, the hook path, hook install, context sources, declared coverage, and adding an agent. |
| [plugin.md](./agents/plugin.md) | Process plugins: loading, the derived spec, canonical hook ingest, probes, invalid-manifest handling at each entry point, and the authoring commands. |
| [adapter_claude.md](./agents/adapter_claude.md), [adapter_codex.md](./agents/adapter_codex.md), [adapter_amp.md](./agents/adapter_amp.md), [adapter_copilot.md](./agents/adapter_copilot.md), [adapter_kimi.md](./agents/adapter_kimi.md), [adapter_pi.md](./agents/adapter_pi.md), [adapter_opencode.md](./agents/adapter_opencode.md), [adapter_antigravity.md](./agents/adapter_antigravity.md), [adapter_cursor.md](./agents/adapter_cursor.md), [adapter_droid.md](./agents/adapter_droid.md), [adapter_kiro.md](./agents/adapter_kiro.md), [adapter_qwen.md](./agents/adapter_qwen.md), [adapter_grok.md](./agents/adapter_grok.md) | One page per built-in adapter: how that agent's hooks, launch and resume, transcript, account, and cost map onto RimZ's types. |
| [providers.md](./agents/providers.md) | Provider accounts and balances: the account model and logins, the out-of-band probe, producer aggregation, window fusion and its caches, refresh cadences, spent windows and parked turns, auto-redeem, auto-continue, and daily dollar caps. |
| [spending.md](./agents/spending.md) | Spend and token pricing: live cost coverage, the cost-history walk, its incremental cache, one walk per namespace, and the price table. |

## The harness

The harness spawns the fleet, addresses it, drives it, and reclaims what it leaves behind. It is a product area rather than one module: most of its code is in `harness/`, and the rest is in `message/`, the top-level `address.rs`, `transcript.rs`, `worktree.rs`, and `trust.rs`, and the CLI verbs that drive them. Start at [fleet.md](./harness/fleet.md), whose code table names the source files behind each page.

| Page | What it owns |
| --- | --- |
| [fleet.md](./harness/fleet.md) | The area map and the launch core: the rules that shape the design, the vocabulary, one launch end to end, the layout IR, the exec wrapper, the address grammar, resume and rebirth, and pane reclamation. |
| [scripting.md](./harness/scripting.md) | Supervised `-p` runs: the durable run record and exit codes, the completion fold, the wake socket and deadlines, verification and retry, the output projections, background joins, and run-pane reclamation. |
| [subagents.md](./harness/subagents.md) | Agent-launched children: the agent-only doorway, what a launch desugars to, fanout, where a child's pane and checkout land, direct-parent stamps, the no-redelegation rule, and the boundary with provider-native subagents. |
| [loops.md](./harness/loops.md) | Loop scheduling: the task and where tasks live, triggers and schedule shapes, elder firing, one fire, run history, signals, watched commands, waits, and the assist log. |
| [budget.md](./harness/budget.md) | Dollar caps: the scopes, where caps and spend come from, the ledgers on disk, the verdict, the human waiver, the park, and the fail-fast gate. |
| [messaging.md](./harness/messaging.md) | Message delivery: the record and its status lifecycle, sending, the delivery pipeline, the pane write, compaction commands, reply waits, scheduling, the inbox verbs, and channels. |
| [transcript.md](./harness/transcript.md) | The durable conversation log: entry kinds, writing entries, causality, the `rimz transcript` projection, and the ask records behind `rimz asks` and `rimz answer`. |
| [worktrees.md](./harness/worktrees.md) | RimZ-owned Git worktrees: the ownership marker, creation and seeding, dirty and landed status, landing on main, removal, and every caller that triggers it. |
| [teams.md](./harness/teams.md) | Team memory: the scratch-file scan, the `blackboard.md` stage board, `rimz teams flip`, and registration re-wakes. |
| [trust.md](./harness/trust.md) | Project trust: the hashed executable surface, launch-time enforcement, grant storage, the stale-grant diff, and every way a grant is made. |

## The sidebar

The sidebar spans two source trees. `sidebar/` is the data plane, the view model a renderer draws; `sidebar_pane/` is the renderer process, including pets. What each zone looks like on screen is [interface/sidebar.md](../interface/sidebar.md).

| Page | What it owns |
| --- | --- |
| [sidebar.md](./sidebar/sidebar.md) | From store to screen: the presence model and binding ladder, ranking and grouping, the cards, process rows, frame composition, the serve loop, reload and repair, and resume on rebirth. |
| [state.md](./sidebar/state.md) | The data plane: renderers and producer election, one fetch cycle, the published lanes, realtime events and push channels, fusion rules, focus intent, cadences, and failure modes. |
| [notifications.md](./sidebar/notifications.md) | Notifications over unread episodes: the producer's policy, the renderer's bell and banner, unread reminders, remote link alerts, handlers, and the trace log. |
| [pets.md](./sidebar/pets.md) | The dashboard pet: from card state to animation, captions, asset loading and its state machine, the cell-art and pixel render tiers, and `rimz list-pets`. |

## Single-file subsystems

Each of these subsystems is one page at the top of `docs/internals/`.

| Page | What it owns |
| --- | --- |
| [theme.md](./theme.md) | The theme core: the four-layer color pipeline, palette resolution and color depth, glyphs, provider identity, the interface language, shared value formats, and the theme boundaries. |
| [store.md](./store.md) | The durable state engine: the on-disk tiers and the workspace record, the event log, the write path and write classes, the read path, session death, maintenance, and what survives what. |
| [sandbox.md](./sandbox.md) | Linux agent mount views: choosing the isolation, bubblewrap preflight, mount order, reachable host paths, environment pins, room tmp, and profile skill views. |
| [multiplexers.md](./multiplexers.md) | The Zellij and tmux seam: backend selection, the `MuxBackend` trait, pane and view identity, reading the room, focus, one sidebar per view, session lifecycle with the `room/` birth, health gate, and reset, both backends, and the Zellij presence plugin. |
| [rimzd.md](./rimzd.md) | The managed `rimzd` view: its panes and how they are specified and identified, the content supervisor, reconciliation and repair, and the loop zone. |
| [remote.md](./remote.md) | SSH attach: targets and aliases, the connect loop and reconnect pacing, terminal hygiene, the connection panel, link health, port forwarding, web tunnels, and bandwidth attribution. |
| [web.md](./web.md) | Browser access: the writable and broadcast ttyd daemons, the trusted-header gate, room attach and the session picker, sharing a room, the credential, the browser client, remote rooms, and the security boundaries. |
| [stats.md](./stats.md) | The `rimz stats` panel: where its figures come from, windows, the heatmap and breakdowns, terminal fitting, the held dashboard, and the machine-readable surfaces. |
| [lsp.md](./lsp.md) | Shared language servers: per-checkout queries, editor attachment, launch-time admission, process leases, memory watchdog, learned cost, and the broker. |
| [diagnostics.md](./diagnostics.md) | Diagnostic evidence: the durable log and its record envelope, the event taxonomy, the frame-stream observer, retention, frame captures, reading an episode, and off-box error reporting. |

## Cost and profiling

These two pages cut across every subsystem above.

| Page | What it owns |
| --- | --- |
| [performance.md](./performance.md) | The cost model: the workload, where work runs, the principles, the cost map, the tick budget, CI counter gates and benchmarks, fleet overhead, anti-patterns, deferred and rejected work, and how to make a performance change. |
| [profiling.md](./profiling.md) | The field guide for measuring a live fleet: what RimZ already publishes, finding the producer, profiling the process, and turning a finding into a guard. |

## From a source tree to its page

Twelve trees under `crates/rimz/src/` carry their own `AGENTS.md` contract, which states the tree's boundaries and links the pages that describe its behaviour. Top-level modules without a contract are indexed in the root [AGENTS.md code map](../../AGENTS.md#code-map).

| Source tree | Page |
| --- | --- |
| `agents/` | [adapter.md](./agents/adapter.md), then the other agent-layer pages |
| `cli/` | No internals page; the commands are in the [CLI reference](../reference/cli.md) and the house rules in [rust-conventions.md](../contributing/rust-conventions.md) |
| `diag/` | [diagnostics.md](./diagnostics.md) |
| `harness/` | [fleet.md](./harness/fleet.md), then the other harness pages |
| `message/` | [messaging.md](./harness/messaging.md) |
| `mux/` | [multiplexers.md](./multiplexers.md) |
| `remote/` | [remote.md](./remote.md) |
| `room/` | [multiplexers.md § Session lifecycle](./multiplexers.md#session-lifecycle) and [rimzd.md](./rimzd.md) |
| `sandbox/` | [sandbox.md](./sandbox.md) |
| `sidebar/` | [state.md](./sidebar/state.md) and [sidebar.md](./sidebar/sidebar.md) |
| `sidebar_pane/` | [sidebar.md](./sidebar/sidebar.md), [notifications.md](./sidebar/notifications.md), and [pets.md](./sidebar/pets.md) |
| `store/` | [store.md](./store.md) |

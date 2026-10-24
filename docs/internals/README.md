# RimZ internals

These pages document how RimZ works, one subsystem at a time, for contributors who read the code. Each page points into `crates/rimz/src/` by path and symbol, and each module's `//!` header stays the authority for its own file. To use RimZ rather than change it, start at the [user documentation](../README.md). To write or change one of these pages, follow the [documentation contract](../AGENTS.md).

Read the shape before a subsystem. [DESIGN.md](../../DESIGN.md) states the attention problem, the design pillars, and the invariants, and [ARCHITECTURE.md](../../ARCHITECTURE.md) gives the runtime shape and the on-disk state. The tables below find a page by topic. Each row names the page's subject and the topics a contributor most often opens it for, and the page's own headings list the rest. When you start from a file instead of a topic, [from source code to its page](#from-source-code-to-its-page) names the page that documents it.

## The agent layer

`agents/` turns each coding agent's native events into one provider-neutral model. It also resolves which account each agent runs on, and prices what the agent spends.

| Page | What it owns |
| --- | --- |
| [model.md](./agents/model.md) | The agent model: status and phase, the rollup and its field lifetimes, the state machine, publishing transitions, the displayed status, activity clocks, and enrichment. |
| [instances.md](./agents/instances.md) | Agent instances: recognizing a hosted CLI, binding a session to a pane, same-pane ownership, launch identity across conversations, instance exit, and session death with supersession. |
| [attribution.md](./agents/attribution.md) | Effort attribution: how a report selects records by lane, lifetime, and branch, how sessions fold into one seat, where each figure comes from, and who counts as a member. |
| [adapter.md](./agents/adapter.md) | The adapter layer: the registry, the spec and the capability traits, the hook path, hook install, context sources, declared coverage, and adding an agent. |
| [plugin.md](./agents/plugin.md) | Process plugins: loading, the derived spec, canonical hook ingest, probes, invalid manifests at each entry point, and the authoring commands. |
| [adapter_claude.md](./agents/adapter_claude.md), [adapter_codex.md](./agents/adapter_codex.md), [adapter_amp.md](./agents/adapter_amp.md), [adapter_copilot.md](./agents/adapter_copilot.md), [adapter_kimi.md](./agents/adapter_kimi.md), [adapter_pi.md](./agents/adapter_pi.md), [adapter_opencode.md](./agents/adapter_opencode.md), [adapter_antigravity.md](./agents/adapter_antigravity.md), [adapter_cursor.md](./agents/adapter_cursor.md), [adapter_droid.md](./agents/adapter_droid.md), [adapter_kiro.md](./agents/adapter_kiro.md), [adapter_qwen.md](./agents/adapter_qwen.md), [adapter_grok.md](./agents/adapter_grok.md), [adapter_hermes.md](./agents/adapter_hermes.md), [adapter_qoder.md](./agents/adapter_qoder.md), [adapter_omp.md](./agents/adapter_omp.md), [adapter_muse.md](./agents/adapter_muse.md), [adapter_minimax.md](./agents/adapter_minimax.md) | One page per built-in adapter: how that agent's hooks, launch and resume, context and transcript, native subagents, account, and cost map onto RimZ's types, and its known gaps. Claude and Codex also cover remote control. |
| [providers.md](./agents/providers.md) | Provider accounts and balances: the account model, the out-of-band probe, window fusion and its caches, refresh cadences, spent windows and paused rows, auto-redeem and auto-continue, which kinds may carry a daily account cap, the per-provider mapping, and adding a provider. |
| [logins.md](./agents/logins.md) | Logins and account homes: which login a room and each agent run on, the default login's exported homes, login sets, agent stamps, and the account-link reconciler. |
| [spending.md](./agents/spending.md) | Spend and token pricing: live cost coverage, the cost-history walk and its incremental cache, one walk per namespace, the price table, and the session role and effort ledgers. |

## The harness

The harness spawns the fleet, addresses it, drives it, and reclaims what it leaves behind. It is a product area that spans several modules. Most of its code is in `harness/`. The rest is in `message/`, the top-level `address`, `transcript`, `worktree`, and `trust` modules, the supervised-run and workflow records in `store/`, and the CLI verbs that drive them. Start at [fleet.md](./harness/fleet.md), whose code table names the source files behind each page.

| Page | What it owns |
| --- | --- |
| [fleet.md](./harness/fleet.md) | The area map and the launch core: the vocabulary, one launch end to end, the layout IR, the exec wrapper, the address grammar, resume and rebirth, and pane reclamation. |
| [scripting.md](./harness/scripting.md) | Supervised `-p` runs: the run record and exit codes, the completion fold, parked runs, the wake socket and deadlines, verification and retry, the output and input projections, background joins, and run-pane reclamation. |
| [subagents.md](./harness/subagents.md) | Agent-launched children: the doorway and what a launch desugars to, fanout, where a child's pane and checkout land, parentage and the permission-mode cap, the no-redelegation rule, response files and the fleet report, joins and cleanup, the backstops, and follow-ups. |
| [loops.md](./harness/loops.md) | Loop scheduling: the task and where it lives, triggers and schedule shapes, host firing, resident tasks, run history, signals, watched commands, waits, host recovery, and the assist log. |
| [workflow.md](./harness/workflow.md) | Workflows: definition rules, run control, the storage boundary, the advance loop, agent attempts, headless judges, wakes and the tick, loop tasks, attention, and accounting. |
| [budget.md](./harness/budget.md) | Dollar caps: the scopes, where each cap and each spend figure comes from, the ledgers on disk, the verdict, the human waiver, the park, the fail-fast gate, and the CLI that edits the ledgers. |
| [hibernate.md](./harness/hibernate.md) | Agent hibernation: admission and doctor checks, provider and helper scopes, selection and the hold order, the freeze and deepen transactions, what a frozen row shows, the wake paths, start reconciliation, and wake diagnostics. |
| [messaging.md](./harness/messaging.md) | Message delivery: the record and its status lifecycle, sending, the delivery pipeline, the pane write, compaction commands, the prompt-cache keepalive, reply waits, scheduling, sender notices, storage and audit, the inbox verbs, channels, and the timing knobs. |
| [transcript.md](./harness/transcript.md) | The durable conversation log: entry kinds and origins, hook writers, causality, the `rimz transcript` projection, and the ask records behind `rimz asks` and `rimz answer`. |
| [worktrees.md](./harness/worktrees.md) | RimZ-owned Git worktrees: the ownership marker, creation and seeding, lifecycle hooks and signals, dirty and landed status, landing on the comparison branch, removal, and every caller that triggers it. |
| [teams.md](./harness/teams.md) | Team boards, stages, and reports: the run archive behind `.rimz/team`, the memory listing, the stage board and its readers, `rimz teams record` and `flip`, the resume re-wake, and `TEAM_REPORT` settlement. |
| [trust.md](./harness/trust.md) | Project trust: the trust states, the hashed executable surface and what stays outside it, launch-time enforcement, every way a grant is made, the stale-grant diff, the trust records, provider folder trust, and adding a command-running field. |

## The sidebar

The sidebar spans two source trees. `sidebar/` is the data plane: it reads, folds, and publishes the view model a renderer draws. `sidebar_pane/` is the renderer: the per-pane supervisor, the per-session host that paints every pane, and the pets. What each zone looks like on screen is [interface/sidebar.md](../interface/sidebar.md).

| Page | What it owns |
| --- | --- |
| [sidebar.md](./sidebar/sidebar.md) | From store to screen: the presence model and binding, ranking and grouping, the cards, process rows, frame composition, the serve loop, reload and repair, and resume on rebirth. |
| [state.md](./sidebar/state.md) | The data plane: the shared room host, one fetch cycle, the published lanes, realtime events, fusion rules, focus intent, cadences, and failure modes. |
| [notifications.md](./sidebar/notifications.md) | Notifications over unread episodes: the host's push policy, the renderer's bell and banner, unread reminders, remote link alerts, handlers and their validation, and the trace log. |
| [pets.md](./sidebar/pets.md) | The dashboard pet and the shared kitty graphics transport: from card state to animation, asset loading, render tiers and capability probes, cell art, the pixel tier, and `rimz list-pets`. |

## Single-file subsystems

Each of these subsystems is one page at the top of `docs/internals/`.

| Page | What it owns |
| --- | --- |
| [theme.md](./theme.md) | The theme core: the four-layer color pipeline and its three renderers, perceptual color math, palette resolution, color depth and degradation, glyphs, provider identity, the interface language, shared value formats, and the theme boundaries. |
| [store.md](./store.md) | The durable state engine: the on-disk tiers and the workspace record, the event log, hook ingress, the write path and write classes, the read path, session death, maintenance, and what survives what. |
| [sandbox.md](./sandbox.md) | Linux agent mount views: choosing the isolation and the subagent cap, provider sandbox flags, bubblewrap preflight, the launch plan, mount order, reachable host paths, environment pins, temp units, the launch reminder, toolchain homes, and profile skill views. |
| [multiplexers.md](./multiplexers.md) | The Zellij and tmux seam: backend selection, the `MuxBackend` trait, pane and view identity, reading the room, focus, one sidebar per view, session lifecycle and room admission, both backends, and the Zellij presence plugin. |
| [rimzd.md](./rimzd.md) | The managed `rimzd` view: its panes and how they are specified and identified, how the rest of the room treats them, the content supervisor, reconciliation, repair, and who repairs when. |
| [remote.md](./remote.md) | SSH attach: targets and aliases, the command RimZ runs on the host and its entry gates, the connect loop and reconnect pacing, terminal hygiene, the connection panel, link health, port forwarding, web tunnels, and bandwidth attribution. |
| [web.md](./web.md) | Browser access: the writable and broadcast daemons, the token gate, room attach and the session picker, sharing a room, the token store, the browser client, configuration, remote rooms, and the security boundaries. |
| [stats.md](./stats.md) | The `rimz stats` surfaces (one-shot report, held dashboard, JSON): where the figures come from, windows, the heatmap, breakdowns, insights, terminal fitting, and the assist rollup. |
| [lsp.md](./lsp.md) | Shared language servers: lifecycle and stop reasons, memory pressure, learned cost, the broker, freshness, editors, state and query usage, configuration, what agents see, the query surface, anchor checks, and the status commands. |

## Debugging and performance

These pages cut across every subsystem above: where the evidence lands when something goes wrong, what a room costs, and how to measure it.

| Page | What it owns |
| --- | --- |
| [diagnostics.md](./diagnostics.md) | Diagnostic evidence: the durable logs and the record envelope, the event taxonomy with each kind's severity and doctor state, retention and frame captures, the frame-stream observer, the focus trace, presence-plugin telemetry, reading the log, investigating an episode, inspecting a live card, and off-box error reporting. |
| [performance.md](./performance.md) | The cost model: the workload, where work runs, the principles, the cost map, fleet overhead, anti-patterns, deferred and rejected work, the guards that hold the budget, and how to make a performance change. |
| [profiling.md](./profiling.md) | The field guide for measuring a live fleet: what RimZ already publishes, finding the host, measuring the process, building a profilable binary, choosing a tool, turning a finding into a guard, and the release baseline. |

## From source code to its page

Twelve trees under `crates/rimz/src/` carry their own `AGENTS.md` contract, which states the tree's boundaries and rules. The table links each contract from the tree's name. A top-level module without a row has no internals page: its `//!` header is its documentation, and the root [AGENTS.md code map](../../AGENTS.md#code-map) places it.

| Source | Page |
| --- | --- |
| `address.rs`, `address/` | [fleet.md § The address](./harness/fleet.md#the-address) |
| [`agents/`](../../crates/rimz/src/agents/AGENTS.md) | [adapter.md](./agents/adapter.md), then the other [agent-layer pages](#the-agent-layer) |
| [`cli/`](../../crates/rimz/src/cli/AGENTS.md) | No internals page. The commands are in the [CLI reference](../reference/cli.md), the shared command grammar in [cli-scenes.md](../contributing/cli-scenes.md), and the house rules in [rust-conventions.md](../contributing/rust-conventions.md). |
| `cli/stats/` | [stats.md](./stats.md) |
| `daemon_view.rs`, `daemon_view/`, `daemon_content.rs` | [rimzd.md](./rimzd.md) |
| [`diag/`](../../crates/rimz/src/diag/AGENTS.md), `observability/` | [diagnostics.md](./diagnostics.md) |
| `disk/` | [store.md § Where the code lives](./store.md#where-the-code-lives) |
| `forge.rs`, `forge/` | [state.md § PR state](./sidebar/state.md#pr-state) |
| [`harness/`](../../crates/rimz/src/harness/AGENTS.md) | [fleet.md](./harness/fleet.md), then the other [harness pages](#the-harness) |
| `lsp/` | [lsp.md](./lsp.md) |
| [`message/`](../../crates/rimz/src/message/AGENTS.md) | [messaging.md](./harness/messaging.md) |
| [`mux/`](../../crates/rimz/src/mux/AGENTS.md) | [multiplexers.md](./multiplexers.md) |
| `proc/cgroup.rs`, `proc/cgroup/` | [hibernate.md § Scopes](./harness/hibernate.md#scopes) |
| [`remote/`](../../crates/rimz/src/remote/AGENTS.md) | [remote.md](./remote.md) |
| `remote_control.rs`, `remote_control/` | [rimzd.md](./rimzd.md), and the remote-control sections of [adapter_claude.md](./agents/adapter_claude.md#remote-control) and [adapter_codex.md](./agents/adapter_codex.md#remote-control) |
| [`room/`](../../crates/rimz/src/room/AGENTS.md) | [multiplexers.md § Session lifecycle](./multiplexers.md#session-lifecycle) and [rimzd.md](./rimzd.md) |
| [`sandbox/`](../../crates/rimz/src/sandbox/AGENTS.md) | [sandbox.md](./sandbox.md) |
| [`sidebar/`](../../crates/rimz/src/sidebar/AGENTS.md) | [state.md](./sidebar/state.md) and [sidebar.md](./sidebar/sidebar.md) |
| [`sidebar_pane/`](../../crates/rimz/src/sidebar_pane/AGENTS.md) | [sidebar.md](./sidebar/sidebar.md), [notifications.md](./sidebar/notifications.md), and [pets.md](./sidebar/pets.md) |
| [`store/`](../../crates/rimz/src/store/AGENTS.md) | [store.md](./store.md). The supervised-run record is in [scripting.md](./harness/scripting.md), and the workflow records in [workflow.md](./harness/workflow.md) |
| `theme/` | [theme.md](./theme.md) |
| `transcript.rs`, `transcript/` | [transcript.md](./harness/transcript.md) |
| `trust.rs`, `trust/` | [trust.md](./harness/trust.md) |
| `wakeup/` | [state.md § Where the code lives](./sidebar/state.md#where-the-code-lives) |
| `web/` | [web.md](./web.md) |
| `workspace.rs`, `workspace/` | [store.md § The workspace record](./store.md#the-workspace-record) |
| `worktree.rs`, `worktree/` | [worktrees.md](./harness/worktrees.md) |

# Agent instances and liveness

This page owns how RimZ joins a live agent pane to the session its hooks report, which session owns a pane when several claim it, and when RimZ declares a session dead. The state each session carries, and how it moves, is [model.md](./model.md). The sidebar's binding ladder and its guards against multiplexer hiccups are [sidebar.md](../sidebar/sidebar.md#presence-model).

An **agent instance** is presence: a live local pane running a known agent command, or hosting one live agent CLI under its pane root, identified by its pane id, `pane_pid`, and process start. A **session** is identity: the `(kind, agent_id)` a hook reports and every durable fact attaches to. The instance exists before any session id is known, and the instance exits when its pane returns to a shell. Joining the two is the one hard problem, and it has three phases: [presence before a session](#before-a-session-binds), [binding](#binding-a-session), and [exit](#instance-exit).

## Two axes

Hook identity and presence are independent, and together they decide how a session binds.

| Axis | Values | What changes |
| --- | --- | --- |
| hook identity | standalone or daemon-routed | A standalone agent runs in its pane, so its hook is a descendant that reads the pane environment and pid and stamps the pane id onto the session. A daemon-routed agent fires its hooks from a background daemon with no pane environment and the daemon's shared pid, so the session arrives unstamped. |
| presence | in-pane or remote | An in-pane agent has a local pane with its own `pane_pid`, so the sidebar can jump to it. A remote agent runs only in a daemon with no local pane (`claude remote-control --spawn worktree`, or a Codex thread started from the web): it has a worktree and nothing to focus. |

Claude, Copilot, and Droid are standalone. Droid 0.170.0 is a narrow exception to direct `$PPID` ownership: its hook emitter is an internal exec worker, so its observations are reassigned to the structurally verified outer TUI pid. Pi and interactive OpenCode run in-process in the pane and are standalone. Codex 0.137 and later routes its hooks through the shared app-server daemon, so even an in-pane Codex session is unstamped ([adapter_codex.md](./adapter_codex.md#session-registration-and-launch-quirks)).

The binding question is therefore one question: does a live local pane bind the session? A stamped session binds by its pane id, an unstamped session binds through recovery, and a session no pane binds is remote. RimZ does not render remote agents yet; it is a known gap ([sidebar.md](../sidebar/sidebar.md#presence-model)). The `claude remote-control` host pane is separate infrastructure, filtered out of the room and surfaced as the `⇅ rc` flag.

## Recognizing a hosted CLI

A multiplexer sometimes reports only a shared runtime basename such as `node` for a pane. The pane producer then walks one bounded chain from the pane root, following a single child at each step, and classifies each full command line through the adapter registry. The outermost proven known CLI supplies the hosted kind and its process start. A chain that is unreadable, branches, lacks a start time, exceeds the depth limit, or matches no adapter supplies nothing. This applies to every adapter, independent of `registers_lazily`, which governs only how an unstamped session is recovered.

## Before a session binds

A wired instance with no bound session renders as an idle `○ <kind>` row, so a just-launched agent reads as itself instead of a bare process ([`idle_agent_row`](../../../crates/rimz/src/store/snapshot/panes/lazy.rs)). Claude shows it at the login screen and in the moment before `SessionStart` stamps the pane. Codex and OpenCode show it before their first real session exists. Kiro stays identity-less until its first-prompt hook or its provider-owned local store yields a safe binding, and Antigravity binds on its first invocation hook or an exact local-session match.

The synthesized row needs an active observation path. Installed hooks activate hook capabilities, and a declared provider-store observation path activates session capabilities; an integration with neither stays a [process row](../sidebar/sidebar.md#process-rows).

## Binding a session

A lifecycle hook arrives with a session id, and RimZ joins it to an instance in one of four ways, most certain first.

1. **Stamped pane.** A standalone hook stamped its pane id, so the join is exact.
2. **Native resume attach.** A resumed launch binds identity and placement before any hook fires. The exec wrapper appends `agent.attached` with the stable launch id it exported, its pane, and the runtime owner of the agent process, without a lifecycle signal. A wrapper that spawns the provider attaches twice, first as itself and then with the provider's pid ([fleet.md](../harness/fleet.md#posture)). Existing cards keep their lifecycle state, a discovered provider session gets an idle seed so it can be addressed before its first hook, and the later hook or local-session observation stays authoritative for lifecycle state.
3. **Recovery for an unstamped session.** Hook ingestion first tries to write a recovered pane stamp from the repaired live frame, for a root `registered` or `turn_started` that carries a worktree path. The snapshot then binds a pane whose command line resumes that exact session (`codex resume <id>`, and the resume syntax the Kiro, Antigravity, and Grok adapters parse). Remaining unstamped sessions pair newest-first with the same-kind, same-directory pane whose process started latest before the session's first event. A session whose stamp names a pane no longer in the frame enters this pairing only when its spec sets `registers_lazily` (Codex, OpenCode, Kiro, Antigravity). A pairing chosen among several viable panes appends a `binding.log.jsonl` breadcrumb. The guards are [sidebar.md](../sidebar/sidebar.md#the-binding-ladder).
4. **Local session observation.** A provider with its own session store binds through that store ([below](#local-session-observations)).

A daemon-routed Codex hook first names the shared app-server daemon as owner. Recovery then re-owns the session to its in-pane CLI process and stores the full pane stamp: pane id, tab id, directory, pane pid, and process start.

### Local session observations

Providers with their own local session stores bind through [`LocalSessionObservation`](../../../crates/rimz/src/agents/mod.rs). Adapters validate and discover the observations; the elected room producer batches admitted workspaces per kind and publishes them; each renderer binds only an exact session-matching publication against its current admitted panes. Caching and revalidation live in [`local_session_cache.rs`](../../../crates/rimz/src/agents/local_session_cache.rs). Stamps there are an optimization only, and unstable or wrong-kind inputs fail closed.

The observation's projection declares how much it may say:

| Projection | Authority |
| --- | --- |
| `IdentityOnly` | Proves session identity and activity bounds. Status, phase, prompt, wait, ask, compaction, context, and lifecycle clocks defer to an exact durable hook row; it synthesizes `idle` only when adopting a provisional row or creating a session with no durable state. |
| `Lifecycle` | Carries a provider-validated fold, overlaid on an exact durable row only when its provider activity is at least as recent as the durable `last_activity`. An accepted provider-native wait is pane-only and clears any durable routable ask. |

Binding runs in a fixed order so a stale fold cannot capture a pane. Exact resume identity binds first. An exact pane and session match consumes both sides before the freshness decision, so a stale fold cannot rebind the pane through a later same-directory candidate. The fresh fallback then skips sessions the runtime projection has ended or expelled for a dead owner, and, for runtime-visible rows, sessions whose durable pane stamp names a pane absent from the live frame.

A fallback on directory alone also needs a positive pane-incarnation clock: the later of the live process start and RimZ's durable launch time. The observation must begin, and remain active, no earlier than that clock. An occupied registered pane is reserved, and a pane with neither clock fails closed until an exact hook, a resume id, or a process-start backfill arrives. Rejections append contained `local_session_bind_rejected` diagnostics, and an exact old stamp contradicted by a newer durable launch appends a `ghost_session_bind` regression signal.

## Same-pane ownership

Several root sessions can claim one pane: a persistent in-process fork, a `/clear` conversation, or a provider that switches conversation ids in place. Ephemeral side conversations (`/side`, `/btw`) are quarantined at ingestion and never reach the agent fold. [`compare_same_pane_owner`](../../../crates/rimz/src/agents/state.rs) picks the one that owns the card:

1. A root holding an open turn (`holds_open_turn`) outranks every rested root.
2. Among open turns, adapter policy decides. `KeepPrimary` picks the earliest registered root, which keeps an open Codex primary ahead of an open persistent in-process fork. `FollowLatest` picks the latest registered root, for providers that switch conversation ids in place.
3. Among rested roots, the latest `last_activity` wins regardless of policy.

`holds_open_turn` reads the context sidecar's rest certificates, so a `running` row with a budget park, a `parked` phase, an active turn error, or a completion or interruption marker counts as rested. Every other same-pane root's activity and estimated active time fold display-only onto the owning card.

Address resolution enforces the same boundary. While a live pane is bound to one session, a different root stamped on that pane is a shadowed audit record: it resolves for no role, kind, name, broadcast, pane, prefix, or exact-session address. Durable history and message audit surfaces keep it.

## Launch identity across conversations

A launch identity belongs to one live agent instance, not to one conversation. Every root proven to share the pane and the agent-process incarnation inherits the launch's routing, team, role, profile, login, channel, and parent linkage, including a fork ([`inherit_launch_identity`](../../../crates/rimz/src/store/snapshot/project.rs)). The proof matches owner pids, which is why the resume attach must name the provider process: a row still owned by the wrapper shares no incarnation with the provider's successor, so the successor inherits nothing.

Inheritance runs after each lifecycle reduction. A successor that registers before the provider-owned attach lands carries no launch identity until its next lifecycle event, such as a turn start or a tool hook; the attach itself does not repair an already-registered successor. Rows that reuse a `launch_id` across a relaunch stay separate, because launch grouping also requires the same instance, and an unstamped row forms its own group.

The inherited identity excludes the card `name`, `name_explicit`, `kind_ordinal`, and `registered_at`. Only a provider-linked compaction continuation takes its predecessor's `registered_at`, so it keeps the open-turn rank the predecessor held.

The ownership rule also selects one un-ended row per launch as its current occupant. Only that row renders and resolves launch-derived handles such as the role; other conversations stay audit records, and a snapshot with a live pane never offers them as message targets. When every row has ended, the most recently active row keeps the launch handle as the audit and resume representative.

These rules apply only after kind, pane incarnation, process identity, directory, and root-session guards establish one live instance. Recovery into an occupied pane prefers unique focus evidence and otherwise admits only one rested same-kind owner; open, ambiguous, already-known, wrong-directory, and wrong-incarnation candidates abstain.

## Instance exit

The in-pane agent process is the liveness truth, read through the pane: the CLI is the pane's foreground process or the single hosted descendant under the pane root, so when it exits the pane returns to a shell and stops reading as an agent. The instance leaves the sidebar on the next snapshot with no exit hook, and there is no `offline` status: a dead agent is a shell row or no row, never a retracted store fact.

The session ends separately. A Claude `SessionEnd` hook stamps the session ended and removes its context sidecar at once; runtime views hide the row, and audit views keep its provider identity for explicit resume within retention. Codex leaves its `SessionEnd` hook unwired ([adapter_codex.md](./adapter_codex.md#hooks-and-lifecycle)), so the [reaper](#session-death) stamps the same state once pane liveness proves the process gone.

## Session death

After a publishing commit, the write-path reaper runs at most once a minute and appends an `ended` observation, stamping `ended_at`, for every root session whose death is provable from the store and the process table ([`store/writer/reap.rs`](../../../crates/rimz/src/store/writer/reap.rs)). The event name records the proof, and [model.md](./model.md#observed-ends-and-reaped-ends) explains why these ends rest an active row at `idle` instead of failing it.

| Event name | Proof |
| --- | --- |
| `ReapedSuperseded` | A newer root of the same kind proves this one yielded its slot ([supersession](#supersession)). |
| `ReapedDead` | The recorded owner process is dead. |
| `ReapedStale` | The session has no agent-process owner (pidless or daemon-owned) and has been inactive past the ghost TTL, 3 hours (`GHOST_SESSION_TTL_SECS`). |
| `WorktreeRemoved` | Worktree removal succeeded, which is affirmative evidence that no matching non-live root can still run there. |

The pid comes from the hook, best-effort, on each lifecycle event: `RIMZ_AGENT_PID=$PPID`, falling back to a process-ancestor walk, plus a platform process-start token that defeats pid reuse. It feeds only the reaper. Stamped-pane binding already keeps a stale agent off a stranger's pane, so the pid never gates rendering.

The persisted live roster protects crash-recovery candidates from `ReapedDead` and `ReapedStale` until room rebirth consumes it. Supersession and worktree removal bypass that protection, because each proves the slot is gone. Room rebirth ends every lost session its recovery plan does not seed, under its own event names (`rimz.recovery-declined`, `rimz.not-resumed`, `rimz.worktree-gone`), which count as observed ends. Already-ended rows are skipped and never supersede an active row, so a repeated reap appends nothing and a retained stamp cannot retire its replacement. A later lifecycle event for the same session id, including a native resume registration, clears the end stamp. Provider-native subagents are never reaped directly; they leave with their parent. This workspace-local convergence complements the cross-workspace `rimz gc`.

Daemon-owned Codex sessions have one faster signal. The app-server loaded-thread reaper drops a daemon-mode session absent from `thread/loaded/list` before the pane fold; an unreachable daemon or an untrusted list keeps every session. An unbound daemon-owned session otherwise abstains from pid liveness and ages through the ghost TTL.

### Supersession

A newer root supersedes an older one of the same kind, with a newer `last_activity`, when any rule in [`session_death.rs`](../../../crates/rimz/src/store/session_death.rs) holds:

| Rule | Condition |
| --- | --- |
| relaunch | Both stamp the same pane and the newer one is a provably different process; or both are paneless remnants of the same worktree and branch. |
| fresh conversation | Both carry fresh rollout lineage on one stamped pane (a `/clear` or `/new`), and the older one does not hold an open turn. |
| in-place switch | A `FollowLatest` adapter reports a distinct newer id on the same pane incarnation and identified agent process, and the older one does not hold an open turn. |
| forked retry | A same-instance fork follows a predecessor already rested by a current provider error or a raw `failed` status. |
| compaction continuation | The newer root names the older as `compacted_from` on the same pane and agent-process incarnation. |

The open-turn guard exists because a child can report a distinct conversation id from the same process mid-turn. So same-process and fresh-lineage switches keep an older owner authoritative while it holds an open turn, while a provably different replacement process supersedes it regardless. The compaction continuation bypasses the guard because the successor names the exact predecessor. A fork has no such link, so it retires only an errored predecessor: a clean `success` or `idle` predecessor, or a raw-active one rested by a completion or interruption certificate, stays a valid sibling. Daemon-owned roots fail every same-instance proof, a known pane or process-start mismatch fails every same-instance proof, and any later hook on the predecessor invalidates an old certificate by advancing its `last_activity`.

Before applying these rules, the writer attaches each raw-`running` or raw-`waiting` root's context sidecar, so a current provider turn error or a completion or interruption settle can certify that an active-looking rollup is actually at rest. The `parked` phase is durable and already in the rollup. A budget park is enrichment, so only readers that fold the budget ledger treat it as rest. A missing or stale sidecar leaves the raw lifecycle status in charge.

Runtime expel and the snapshot-time view reap ([`store/snapshot/view/reap.rs`](../../../crates/rimz/src/store/snapshot/view/reap.rs)) apply the store-only liveness and the same supersession rules as latency shims during the debounce window. Only the durable writer reads sidecars, so only it has the provider-rest evidence.

## See also

- [model.md](./model.md): the rollup, the state machine, and the displayed status these sessions carry.
- [sidebar.md](../sidebar/sidebar.md#presence-model): row presence, the binding ladder, and honest reads across a multiplexer hiccup.
- [fleet.md](../harness/fleet.md#resume-and-rebirth): resume planning, rebirth, and the exec wrapper's attach.
- [worktrees.md](../harness/worktrees.md): worktree removal, which retires matching sessions.
- [multiplexers.md](../multiplexers.md): pane identity and process reads behind the instance.

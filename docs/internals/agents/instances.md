# Agent instances and liveness

This page owns how RimZ joins a live agent pane to the session its hooks report, which session owns a pane when several claim it, and when RimZ declares a session dead. The state each session carries, and how it moves, is [model.md](./model.md). The sidebar's binding ladder and its guards against multiplexer hiccups are [sidebar.md](../sidebar/sidebar.md#presence-model).

An **agent instance** is presence: a live local pane running a known agent command, or hosting one live agent CLI under its pane root, identified by its pane id, `pane_pid`, and process start. A **session** is identity: the `(kind, agent_id)` a hook reports and every durable fact attaches to. The instance exists before any session id is known, and the instance exits when its pane returns to a shell. Joining the two is the one hard problem, and it has three phases: [presence before a session](#before-a-session-binds), [binding](#binding-a-session), and [exit](#instance-exit).

## Two axes

Hook identity and presence are independent, and together they decide how a session binds.

| Axis | Values | What changes |
| --- | --- | --- |
| hook identity | standalone or daemon-routed | A standalone agent runs in its pane, so its hook is a descendant that reads the pane environment and pid and stamps the pane id onto the session. A daemon-routed agent fires its hooks from a background daemon with no pane environment and the daemon's shared pid, so the session arrives unstamped. |
| presence | in-pane or remote | An in-pane agent has a local pane with its own `pane_pid`, so the sidebar can jump to it. A remote agent runs only in a daemon with no local pane (`claude remote-control --spawn worktree`, or a Codex thread started from the web): it has a worktree and nothing to focus. |

Codex is the only adapter that sets `daemon_hooked_sessions`, and even Codex is standalone when RimZ launches it: managed launches run embedded through `--no-daemon`, with pane-stamped hooks. A user-run `codex` or a phone-started background session can still arrive unstamped ([adapter_codex.md](./adapter_codex.md#session-registration-and-launch-quirks)). Every other adapter's hook runs inside the pane. A few adapters correct which process the hook names as owner, so that liveness follows the pane's TUI: Droid's internal exec worker ([adapter_droid.md](./adapter_droid.md)) and OpenCode's private serve child ([adapter_opencode.md](./adapter_opencode.md)).

The binding question is therefore one question: does a live local pane bind the session? A stamped session binds by its pane id, an unstamped session binds through recovery, and a session no pane binds is remote. Remote agents do not render, a known gap that [sidebar.md](../sidebar/sidebar.md#presence-model) owns. The `claude remote-control` host pane is separate infrastructure, filtered out of the room and surfaced as the `⇅ rc` flag.

## Recognizing a hosted CLI

A multiplexer sometimes reports only a shared runtime basename such as `node` for a pane. The pane producer then walks one chain from the pane root, following a single child at each step, and classifies each full command line through the adapter registry ([`proc/pane_probe.rs::hosted_agent_process_for_root`](../../../crates/rimz/src/proc/pane_probe.rs)). The outermost known CLI supplies the hosted kind, its process start, and the lineage: the pids on the chain below the root, down to the CLI. A chain that is unreadable, branches, lacks a start time, or matches no adapter within `PANE_AGENT_DESCENT_DEPTH` (8) steps supplies nothing. An empty command line, such as a process mid-`execve` or a zombie, counts as unreadable. The walk runs for every adapter. `registers_lazily` plays no part in it, and only changes how a stamp is validated and how an unstamped session is recovered.

A RimZ-launched pane's root is its [resident wrapper](../harness/fleet.md#resident-wrapper). Its command line, `rimz agents exec <kind> …` (the parked image adds `--supervise <path>`), itself classifies as the kind, so the walk stops at the root and never inspects the wrapper's children. The wrapper's helper children, a parked run's duties and a scoped launch's hook drainer, therefore cost nothing here. That protection holds only while the wrapper's command line classifies: the program basename must be `rimz` and `agents exec <kind>` must come first ([`proc/command.rs::rimz_exec_kind`](../../../crates/rimz/src/proc/command.rs)). A renamed binary, or a global flag before `agents`, leaves the root unclassified, and the walk then abstains at the wrapper's branch.

## Before a session binds

A wired instance with no bound session renders as an idle `○ <kind>` row, so a just-launched agent reads as itself instead of a bare process ([`store/snapshot/panes/lazy.rs::idle_agent_row`](../../../crates/rimz/src/store/snapshot/panes/lazy.rs)). A kind is wired when its spec declares `local_session_discovery`, or when it has a hook install and its hooks are installed ([`sidebar/agent_projection.rs::probe_adapters`](../../../crates/rimz/src/sidebar/agent_projection.rs)). A recognized pane of an unwired kind stays a [process row](../sidebar/sidebar.md#process-rows).

How long the idle row lasts is provider behaviour, and each adapter page records it. Claude shows the row at its login screen and until `SessionStart` stamps the pane. Codex registers at its first prompt ([adapter_codex.md](./adapter_codex.md#session-registration-and-launch-quirks)), and Hermes fires `on_session_start` at its first turn ([adapter_hermes.md](./adapter_hermes.md)). Kiro and Antigravity stay identity-less until a hook or their provider-owned local store yields a safe binding ([adapter_kiro.md](./adapter_kiro.md), [adapter_antigravity.md](./adapter_antigravity.md)).

## Binding a session

A lifecycle hook arrives with a session id, and RimZ joins it to an instance in one of four ways, most certain first.

1. Stamped pane. A standalone hook stamped its pane id, so the join is exact.
2. Native resume attach. A resumed launch binds identity and placement before any hook fires. The exec wrapper attaches the session as itself, stamps it with a `rimz.agent-resumed` registration, spawns the provider, and attaches again with the provider's pid ([fleet.md](../harness/fleet.md#posture) owns the sequence). The registration revives an ended card at `idle` and sets `resumed_at`, which the next event on that card clears. A discovered provider session gets an idle seed so it can be addressed before its first hook. The later hook or local-session observation is authoritative for lifecycle state.
3. Recovery for an unstamped session. Hook ingestion first tries to write a recovered pane stamp from the repaired live frame, for a root `registered` or `turn_started` that carries a worktree path ([`store/snapshot/panes/lazy.rs::HookPaneRecoveryContext`](../../../crates/rimz/src/store/snapshot/panes/lazy.rs)). The snapshot then binds a pane whose command line resumes that exact session (`codex resume <id>`, and the resume syntax the Kiro, Antigravity, and Grok adapters parse). It pairs the remaining unstamped sessions to same-kind, same-directory panes by process-start proximity. A session whose stamp names a pane absent from the frame enters that pairing only when its spec sets `registers_lazily`. The guards and the pairing fallback are [sidebar.md](../sidebar/sidebar.md#the-binding-ladder).
4. Local session observation. A provider with its own session store binds through that store ([below](#local-session-observations)).

`registers_lazily` is set by Codex, OpenCode, Kiro, Antigravity, Muse, and MiniMax, and a process plugin sets it in its manifest. Besides admitting a dead-pane stamp to recovery, it adds kind, activity, and process-start guards to stamp validation ([sidebar.md](../sidebar/sidebar.md#the-binding-ladder), rung 1).

Hook recovery into a pane another live session occupies is narrower ([`HookPaneRecoveryContext::occupied_candidate_pool`](../../../crates/rimz/src/store/snapshot/panes/lazy.rs)). Only a daemon-hooked kind (Codex) or a `FollowLatest` kind may adopt an occupied pane, and only for a session not yet stamped. A daemon-hooked session adopts at its first turn start. A `FollowLatest` session may adopt at registration or at a turn start. Candidates with client focus come first, and a `FollowLatest` kind still needs the focused candidate's owner to be resting. Without focus, a `Fresh` or `FollowLatest` session takes the one candidate whose owner is resting. For a non-`FollowLatest` kind, that owner must itself be `Fresh`. Wrong-directory, wrong-kind, and later-started candidates abstain, and so does a choice among several.

A recovered stamp carries the full pane: pane id, tab id, directory, pane pid, and process start. When recovery can also prove the in-pane CLI's pid from the pane pid, it re-owns the session to that process. A daemon-routed Codex hook names the shared app-server daemon as owner, so this proof moves the session onto its in-pane CLI. Without the proof, the session keeps its existing owner. Every attempt appends a breadcrumb to `audit/binding.log.jsonl`.

### Local session observations

Providers with their own local session stores bind through [`agents/mod.rs::LocalSessionObservation`](../../../crates/rimz/src/agents/mod.rs). Adapters validate and discover the observations, the room host batches admitted workspaces per kind and publishes them, and each renderer binds a publication against its current admitted panes ([`store/snapshot/view/live.rs::SidebarSnapshot::with_local_sessions_and_diagnostics`](../../../crates/rimz/src/store/snapshot/view/live.rs)). Caching and revalidation live in [`agents/local_session_cache.rs`](../../../crates/rimz/src/agents/local_session_cache.rs). Stamps there are an optimization only, and unstable or wrong-kind inputs fail closed.

Discovery is account-scoped. Every discovery outside an adapter goes through `ProviderLogin::local_sessions` ([`agents/login.rs`](../../../crates/rimz/src/agents/login.rs)), which reads the provider home of one account and stamps each observation with it (`default` is absent). A row seeded from an observation takes that account. An exact or provisional row the observation binds to keeps its own account and its parent link, so a launched child that binds before its first `SessionStart` stays nested under its parent.

The observation's projection declares how much it may say:

| Projection | Authority |
| --- | --- |
| `IdentityOnly` | Proves session identity and activity bounds. Status, phase, prompt, wait, ask, compaction, context, and lifecycle clocks defer to an exact durable hook row. It synthesizes `idle` only when it adopts a provisional row or creates a session with no durable state. |
| `Lifecycle` | Carries a provider-validated fold, overlaid on an exact durable row only when its provider activity is at least as recent as the durable `last_activity`. An accepted provider-native wait is pane-only and clears any durable routable ask. |

Binding runs in a fixed order, so a stale fold cannot capture a pane. Each observation tries these in turn and stops at the first that applies:

1. A durable row of the same kind and session stamped on a live pane in that workspace binds it. The match consumes both the pane and the observation before freshness counts, so a stale fold cannot rebind the pane through a later same-directory candidate.
2. A pane whose command line resumes that exact session binds it.
3. A durable row of that session that shares a pane without owning it holds the observation out of the fallback and leaves the pane to its owner.
4. A session the runtime projection has ended, or expelled for a dead owner, is skipped.
5. A runtime-visible row whose durable stamp names a pane absent from the live frame is held, so a degraded frame delays its binding by one cycle instead of moving it.
6. The fresh fallback takes the remaining observations newest first. Each binds the unique closest free same-directory pane that the incarnation clock admits and that resumes no other session.

The incarnation clock keeps a fresh fold off a pane that a newer launch owns ([`store/snapshot/view/live.rs::fresh_pane_allows_bind`](../../../crates/rimz/src/store/snapshot/view/live.rs)). The pane's newest launched row, root or launched child, decides first. When that row is the observation's own session, the bind needs no clock: its stamp already matched the pane. A fork successor's rollout can predate its first hook, and [inherited launch identity](#launch-identity-across-conversations) excludes `registered_at`, so a clock from that stamp would reject the successor's own rollout. When the row is another real session of the kind, the pane is reserved and the bind fails. Provisional launches still admit adoption. Every other bind needs a positive clock: the later of the live process start and that launch's `registered_at`. The observation must begin, and stay active, no earlier than that clock. Without either clock the fallback fails closed until an exact hook, a resume id, or a process-start backfill arrives.

Two diagnostics record the edges. A `local_session_bind_rejected` diagnostic is appended only when the fallback binds nothing, and `legacy_fresh_pane_allows_bind` (the process-start check alone, with no launch reservation and a pass for a pane with no start) would have bound exactly one pane that the incarnation clock rejects. A busy same-directory room therefore records no diagnostic for each rejected historical rollout. A `ghost_session_bind` regression signal is appended when an exact or co-resident match names a session that predates the pane's newer durable launch.

## Same-pane ownership

Several root sessions can claim one pane: a persistent in-process fork, a `/clear` conversation, or a provider that switches conversation ids in place. [`agents/state.rs::AgentState::compare_same_pane_owner`](../../../crates/rimz/src/agents/state.rs) picks the one that owns the card:

1. A root that holds an open turn (`holds_open_turn`) outranks every rested root.
2. Among open turns, adapter policy decides. `KeepPrimary` picks the earliest registered root, which keeps an open Codex primary ahead of an open persistent in-process fork. `FollowLatest` picks the latest registered root, for providers that switch conversation ids in place: OpenCode, Cursor, Antigravity, and OMP.
3. Among rested roots, the latest `last_activity` wins regardless of policy.

`holds_open_turn` reads the context sidecar's rest certificates, so every snapshot that compares pane owners or resolves an address attaches them first. A `running` row counts as rested when it carries a budget park, a `parked` phase, an active turn error, or a completion or interruption certificate. A current native-wait or plan-proposed certificate overrides all of these and keeps the turn open. A `waiting` row holds an open turn unless an interruption certificate rests it. Every other status is rested.

The owning card shows the latest activity of the other roots stamped on its pane ([`store/snapshot/view/aggregate/forks.rs::fold_same_pane_activity_onto_bound_row`](../../../crates/rimz/src/store/snapshot/view/aggregate/forks.rs)). The fold is display-only, and it skips an owning card that is waiting, failed, or turn-dead.

Ephemeral side conversations (`/side`, `/btw`) never reach the agent fold. Ingestion quarantines them, and the fold remembers only the side id. Each side hook resolves its host at ingestion from the merged rollup, log-rotation carryover included ([`store/session_death.rs::side_conversation_host`](../../../crates/rimz/src/store/session_death.rs)). The host is the root that `compare_same_pane_owner` picks among the live roots on the hook's pane and agent process that no sibling supersedes. The hook advances the host's activity and estimated active time. A side turn opens the host's span when that span is closed, and a side turn end closes it only while the host's own turn is not running.

Address resolution enforces the same boundary ([`address.rs::shadowed_by_pane_owner`](../../../crates/rimz/src/address.rs)). While a live pane is bound to one session, a different root stamped on that pane is a shadowed audit record. It resolves for no role, kind, name, broadcast, pane, prefix, or exact-session address. Durable history and the message audit surfaces keep it.

## Launch identity across conversations

A launch identity belongs to one live agent instance, and every conversation that instance hosts shares it. A root proven to share the pane and the agent-process incarnation inherits the launch's routing, team, role, profile, login, channel, and parent linkage, a fork included ([`store/snapshot/project.rs::inherit_launch_identity`](../../../crates/rimz/src/store/snapshot/project.rs) lists every field). The proof matches pane id and owner pid, with their start tokens, and only for an agent-process owner ([`store/session_death.rs::same_instance_placement`](../../../crates/rimz/src/store/session_death.rs)). It does not compare directories. The successor inherits from the earliest registered same-instance row that holds a launch id. This is why the resume attach must name the provider process: a row still owned by the wrapper shares no incarnation with the provider's successor, so the successor inherits nothing.

Inheritance runs after each lifecycle reduction, and only for a successor with a pane, an agent-process owner, and no launch id of its own. A successor that registers before the provider-owned attach lands carries no launch identity until its next lifecycle event, such as a turn start or a tool hook. The attach itself does not repair an already-registered successor. Rows that reuse a `launch_id` across a relaunch stay separate, because launch grouping also requires the same instance, and a row without a launch id forms its own group ([`address.rs::launch_groups`](../../../crates/rimz/src/address.rs)).

A row rebound to a new launch keeps answering to its earlier launch ids through `prior_launch_ids`, so its pre-close children and peers stay linked. A picker launch merges into a known session when that session is paneless or ended, even if the ended row keeps its old pane stamp. A live pane-stamped session is not taken over.

A launch id can be held by several conversations of one instance at once (a fork, a `/clear`, a compaction successor). A change that maps a launch id to one row must say which of them wins. [`address.rs::launch_row`](../../../crates/rimz/src/address.rs) prefers a live row that holds the id as its current `launch_id` over a live row that answers to it only as an earlier id. A successor still running on the id therefore keeps resolving to itself after its predecessor is revived. Child links stay broader: a child stamped with that id matches both rows through `AgentState::parent_is`.

The inherited identity excludes the card `name`, `name_explicit`, `kind_ordinal`, and `registered_at`. Only a provider-linked compaction continuation takes its predecessor's `registered_at`, so it keeps the open-turn rank the predecessor held.

The ownership rule also selects one un-ended row per launch as its current occupant. Only that row renders and resolves launch-derived handles such as the role. Other conversations stay audit records, and a snapshot with a live pane never offers them as message targets. When every row has ended, the most recently active row keeps the launch handle as the audit and resume representative.

## Instance exit

The in-pane agent process is the liveness truth, read through the pane. The CLI is the pane's foreground process or the single hosted descendant under the pane root, so when it exits the pane returns to a shell and stops reading as an agent. The instance leaves the sidebar on the next snapshot with no exit hook. There is no `offline` status: a dead agent is a shell row or no row, never a retracted store fact.

The recorded owner process narrows visibility from the other side. The runtime projection hides a row whose owner is known dead before any reap ends it ([`store/runtime.rs::agent_is_runtime_visible`](../../../crates/rimz/src/store/runtime.rs)). Liveness only suppresses: an unknown owner abstains, and a live owner never admits a row that no pane binds.

The session ends separately. A Claude `SessionEnd` hook stamps the session ended and removes its context sidecar at once. Runtime views hide the row, and audit views keep its provider identity for explicit resume within retention. Codex leaves its `SessionEnd` hook unwired ([adapter_codex.md](./adapter_codex.md#hooks-and-lifecycle)), so the [reaper](#session-death) stamps the same state once the process is gone.

## Session death

After a publishing commit, the write-path reaper runs at most once a minute ([`store/writer/reap.rs`](../../../crates/rimz/src/store/writer/reap.rs)). It appends an `ended` observation, stamping `ended_at`, for every root session whose death is provable from the store and the process table. The event name records the proof, and [model.md](./model.md#observed-ends-and-reaped-ends) explains why these ends rest an active row at `idle` instead of failing it.

| Event name | Proof |
| --- | --- |
| `ReapedSuperseded` | A newer root of the same kind proves this one yielded its slot ([supersession](#supersession)). |
| `ReapedDead` | The recorded owner process is dead. For a daemon-owned session that is the daemon. |
| `ReapedStale` | The session has no agent or script owner (no pid, or a daemon owner) and has been inactive past the ghost TTL, 3 hours (`store/session_death.rs::GHOST_SESSION_TTL_SECS`). |
| `WorktreeRemoved` | Worktree removal succeeded, which is affirmative evidence that no matching non-live root can still run there. |

The owner comes from the hook, best-effort, on each lifecycle event: `RIMZ_AGENT_PID`, which hook configurations set to `$PPID`, falling back to a process-ancestor walk, plus a platform process-start token that defeats pid reuse ([`cli/hooks/owner.rs`](../../../crates/rimz/src/cli/hooks/owner.rs)). The resulting `RuntimeOwner` feeds the reaper, runtime visibility, and every same-instance proof: supersession, launch-identity inheritance, launch grouping, and the stamped-root activity waiver. It never binds a pane by itself. Stamped-pane binding is what keeps a stale agent off a stranger's pane.

A provider that forks on resume is ended at write time, ahead of the reaper. The resume stamp sets `resumed_at` on the card, and the card's own next event clears it. While it is set, a root lifecycle event from another session of the kind, on the card's pane and agent process, makes the lifecycle writer append `ReapedSuperseded` for the resumed card in the same batch. The provider-owned attach applies the same rule to a fork that registered first ([`store/writer/lifecycle.rs::superseded_on_attach`](../../../crates/rimz/src/store/writer/lifecycle.rs)).

Two records shield crash-recovery candidates. The persisted live roster protects them from `ReapedDead` and `ReapedStale` until room rebirth consumes it. Supersession bypasses the roster. The pending-recovery record protects a root against every reap reason, and against superseding another agent. The protection lasts until a settlement confirms the agent's tab open or ends it, or until a wrapper attaches a parked agent that is not seeded awaiting tab confirmation. Pending launched children get no reap shield, because no settlement asks about them. The record still protects every pending agent against the old exec wrapper's inferred `rimz.agent-ended` ([fleet.md](../harness/fleet.md#reclaiming-a-pane)). Worktree removal bypasses both records, because it proves the slot is gone.

A recovery settlement ends sessions under its own event names, which count as observed ends:

| Event name | Ends |
| --- | --- |
| `rimz.recovery-declined`, `rimz.not-resumed`, `rimz.worktree-gone` | A lost root session the user declined or dropped in an attended settlement. |
| `rimz.child-not-resumed` | A non-live launched child, in every settlement, attended or not, after its newest open runs are canceled. Each durable child end emits an informational `recovery_child_ended` diagnostic. |
| `rimz.seat-refilled` | A parked team member whose seat a live replacement already holds, or whose planned Fresh replacement's tab is confirmed open. An unconfirmed replacement leaves the old session pending. |

Ends are idempotent and reversible. Already-ended rows are skipped and never supersede an active row, so a repeated reap appends nothing and a retained stamp cannot retire its replacement. A later lifecycle event for the same session id, a native resume registration included, clears the end stamp. Provider-native subagents are never reaped directly: they leave with their parent. This workspace-local convergence complements the cross-workspace `rimz gc`.

Daemon-owned Codex sessions have one faster signal, and it is a snapshot-time drop, never a durable end ([`store/snapshot/view/reap.rs`](../../../crates/rimz/src/store/snapshot/view/reap.rs)). The loaded-thread reaper drops a daemon-owned session absent from `thread/loaded/list` when the session is paneless or its stamped pane is absent from the admitted frame. A live stamped pane protects it. An unreachable daemon, an untrusted list, or no running daemon keeps every session. While its daemon lives, an unbound daemon-owned session abstains from pid liveness and ages through the ghost TTL.

### Supersession

A newer root supersedes an older one of the same kind, with a newer `last_activity`, when any rule in [`store/session_death.rs::supersedes`](../../../crates/rimz/src/store/session_death.rs) holds:

| Rule | Condition |
| --- | --- |
| relaunch | Both stamp the same pane and the newer one is a provably different process (two known, distinct owner pids). Or both are paneless remnants of the same worktree and branch. |
| fresh conversation | Both carry `Fresh` rollout lineage on one stamped pane (a `/clear` or `/new`), and the older one does not hold an open turn. |
| in-place switch | A `FollowLatest` adapter reports a distinct newer id on the same pane incarnation and identified agent process, and the older one does not hold an open turn. |
| forked retry | A same-instance fork follows a predecessor already rested by a current provider error or a raw `failed` status. |
| compaction continuation | The newer root names the older as `compacted_from` on the same pane and agent-process incarnation. |

The open-turn guard exists because a child can report a distinct conversation id from the same process mid-turn. Same-process and fresh-lineage switches therefore keep an older owner authoritative while it holds an open turn, while a provably different replacement process supersedes it regardless. The compaction continuation bypasses the guard because the successor names the exact predecessor. A fork has no such link, so it retires only an errored predecessor. A clean `success` or `idle` predecessor, or a raw-active one rested by a completion or interruption certificate, stays a valid sibling. Daemon-owned roots fail every same-instance proof, and so does a known pane or process-start mismatch. Any later hook on the predecessor invalidates an old certificate by advancing its `last_activity`.

Before applying these rules, the writer attaches each raw-`running` or raw-`waiting` root's context sidecar, so a current provider turn error or a completion or interruption settle can certify that an active-looking rollup is at rest. The `parked` phase is durable and already in the rollup. A budget park is enrichment, so only readers that fold the budget ledger treat it as rest. A missing or stale sidecar leaves the raw lifecycle status in charge.

Runtime expel and the snapshot-time view reap ([`store/snapshot/view/reap.rs`](../../../crates/rimz/src/store/snapshot/view/reap.rs)) apply the store-only liveness and the same supersession rules as latency shims during the debounce window. They judge rest from whatever certificates their snapshot carries, and a snapshot without them reads the raw lifecycle status.

## See also

- [model.md](./model.md): the rollup, the state machine, and the displayed status these sessions carry.
- [sidebar.md](../sidebar/sidebar.md#presence-model): row presence, the binding ladder, and honest reads across a multiplexer hiccup.
- [fleet.md](../harness/fleet.md#resume-and-rebirth): resume planning, rebirth, and the exec wrapper's attach.
- [worktrees.md](../harness/worktrees.md): worktree removal, which retires matching sessions.
- [multiplexers.md](../multiplexers.md): pane identity and process reads behind the instance.

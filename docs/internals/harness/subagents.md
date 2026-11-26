# Agent-launched subagents

> One agent delegating a bounded prompt to another. This page owns what a `rimz subagents` child adds to a supervised run: the agent-only doorway, what a launch desugars to, where the child's pane and checkout land, the direct-parent stamp and the rule that children cannot delegate, which children each verb selects, the fleet report that brings settled answers back to the parent, the launcher-opened peer turns that share it, and the backstops that end a child with its parent. The supervised run underneath a child is [scripting.md](./scripting.md), the launch core it rides on is [fleet.md](./fleet.md), and the user-facing flags are [cli/subagents.md](../../reference/cli/subagents.md).

## Two things are called a subagent

RimZ shows two different mechanisms under one product term and nests both under the parent's card. They carry different truth, so keep them apart.

A **provider-native subagent** is the agent's own child, which the provider runs inside its own turn. RimZ learns it exists only from `SubagentStarted` and `SubagentStopped` hook signals and folds it into the rollup as a child row. It has no pane, no run record, and no address. [model.md](../agents/model.md#subagents) owns it.

A **launched subagent** (on this page, a launched child) is a full RimZ agent that `rimz subagents` creates: its own pane, provider process, durable run record, petname, launch profile, transcript, and address. The petname is its address, and the sidebar card labels it by launch profile. `rimz transcript @<petname>` reads its conversation, and channel and `@all` transcript views leave it out. Its spend folds into the parent's figures in the sidebar, `rimz agents show`, `rimz teams list` and `rimz teams show`, and attribution, beside provider-native spend. Attribution groups it under the profile of its newest continuation ([attribution.md](../agents/attribution.md#the-subagent-breakdown)). This page owns the launched form.

One field combination separates them. Both predicates live on `AgentState` in [`agents/state.rs`](../../../crates/rimz/src/agents/state.rs):

| Predicate | Test | Means |
| --- | --- | --- |
| `is_launched_child` | `parent_agent_id.is_some() && launch_depth.is_some()` | `rimz subagents` launched it, so it has a pane and a run |
| `is_provider_subagent` | `parent_agent_id.is_some() && launch_depth.is_none()` | the provider launched it inside its own turn |

A peer launched through `rimz agents` carries `launch_depth` without a parent and matches neither predicate. When an agent launched it, the peer also carries `launched_by` (the caller's kind and launch id). `launched_by` routes the [fleet report](#the-fleet-report), the enrollment of launcher messages into [peer turns](#peer-turns), and the transcript author of the launcher's text (`cli/hooks/lifecycle/transcript.rs`). Nesting, reaping, cascade stop, and resume ignore it. `AgentState::launcher` returns the parent link for a launched child and `launched_by` otherwise, and `launcher_is` matches it the way `parent_is` does. Every caller-scoped verb on this page filters provider-native rows out.

Both kinds attach to a parent card through one chokepoint, `AgentState::parent_is`. The child's `parent_agent_id` must equal one of the candidate's identities (its `agent_id`, its `launch_id`, or a prior launch id, through `AgentState::identity_ids`), and `parent_agent_kind` (defaulting to the child's own kind) must equal the candidate's kind. A launched child's link names its caller's launch ([parentage](#launch-generations-and-parentage)). A provider-native link names the parent's session id, and the store writer flattens deeper provider ancestry as it adopts hook observations. The sidebar's subagent section is therefore origin-blind and one level deep.

A launched child renders once. The pane projection ([`store/snapshot/panes.rs`](../../../crates/rimz/src/store/snapshot/panes.rs)) binds a pane whose agent carries a parent as a nested agent, which records the pane without emitting a top-level row, as long as some row of the parent's launch renders. When no row of that launch renders and the child is still live, the child is promoted to its own top-level row, so a live pane never renders nowhere.

The two also differ in reach. A launched child is a peer for `rimz message` and `rimz pane` even while it renders under a parent card. A provider-native child is display-only.

## The doorway

Launch, `fanout`, `wait`, and `stop` are agent-only ([`cli/subagents/mod.rs`](../../../crates/rimz/src/cli/subagents/mod.rs), `resolve_agent_caller`). The gate passes when the process carries the RimZ agent-kind launch environment, or, without it, when process ancestry matches a live durable agent. A user shell that passes neither gets an error pointing at `rimz subagents list`, `rimz agents`, and `rimz teams`. The gate reads only whether the environment is present. A stale environment is refused later, when launch ancestry or `wait` and `stop` resolve the caller's durable row ([caller resolution](#launch-generations-and-parentage)).

`list` and `profiles` are read-only and open to any caller. Bare `rimz subagents` with no profile, or a blank one, refuses with a usage error (exit 2) before the caller gate, so it also answers any caller. `profiles` resolves optional caller context from existing room state without creating it. `list` selects the caller's children when the caller resolves to a durable agent row, and the current channel's children otherwise ([who counts](#who-counts-as-a-launched-child)).

The doorway is a usability boundary, not a security one. It uses the supervised background runner of `rimz agents <profile> <prompt> -p --bg` and adds the child deadline ladder. `subagents` exists so a delegating agent does not have to choose supervision flags, and so every child launched through it is supervised, background, deadlined, and self-cleaning.

## What a launch desugars to

`rimz subagents <profile> <prompt>` builds an ordinary `AgentLaunchArgs` in `SubagentLaunchArgs::into_agent_launch` and hands it to `launch_supervised_background`, the same background launcher a fanout uses. Its request starts from `crates/rimz/src/harness/run.rs::SupervisedRunRequest::subagent`, which sets the fields every subagent launch shares, and the launch itself is `crates/rimz/src/launcher/mod.rs::launch_background`. The `subagent` MCP tool of the `rimz-subagents` capability builds its request from the same constructor and calls the same function, in the server's process ([mcp.md](../mcp.md#starting-things)); it takes the profile, the prompt, and the optional description, cwd, timeout, model, effort, and agent, and leaves every other flag to the command. The arguments carry only the ladder values the launch set (`SupervisedRunRequest.warn` is optional so `--warn none` stays distinct from unset). Supervised preparation fills the rest once the profile resolves: `crates/rimz/src/launcher/supervised.rs::resolve_deadline_ladder` takes the launch value, then the definition's `timeout:` (carried on `AgentCell.timeout` through chain resolution), then `[agents.subagents]`, filters the warn offsets against the final timeout, and refuses a malformed table string (naming its `agents.subagents.<key>`) or an overflowing kill bound before any run record or pane exists. The resolved values reach the run record where they always did.

| Field | Value | Why |
| --- | --- | --- |
| `print` | always `true` | selects RimZ's supervised run, not provider print mode, so interactive providers can take follow-up turns |
| `bg` | always `true` | single launch and fanout share one background composition |
| `subagent` | always `true` | selects the parent stamp, profile namespace, pane zone, and no-delegation rules below |
| `self_cleanup_on_completion` | always `true` | the wrapper stops the provider once the report is queued or joined and the child rests unfocused for `[agents.subagents] linger`, then closes the pane. Supervised preparation stores `self_cleanup_on_completion && !keep` as the leader's `cleanup` (`crates/rimz/src/launcher/supervised.rs::prepare`) |
| `keep` | `--keep`, default false | turns automatic completion cleanup off and holds the pane past provider exit until the parent ends or Ctrl-C, under a live parent watch |
| `timeout` | `--timeout`, else the profile's `timeout:`, else `[agents.subagents] timeout`, default `30m` | soft work bound for pending or running work, never for a lingering terminal run |
| `warn` | `--warn`, else `[agents.subagents] warn`, default `["10m", "5m"]` | offsets before the soft bound, normalized against the final timeout |
| `grace` | `--grace`, else `[agents.subagents] grace`, default `5m` | reporting time before the producer kills the run. Zero is stored absent |
| `report_to` | `launcher` | see [which runs a fleet owes](#which-runs-a-fleet-owes) for `nobody` |
| join | none unless `--wait[=DURATION]` | the parent keeps moving: the launcher's join returns at once ([joins](#joins-stops-and-the-digest)), so the duration has no effect |

The pass-through fields are `--prompt-file`, `--model`, `--tier`, `--agent` (inheriting another profile from `subagents/` or `agents/`, or a kind, below the child profile), `--effort`, `--fast`/`--no-fast`, `--isolation`, `--yolo`, `--cwd`, `--add-dir`, and `--empty-dir` ([below](#the-child-works-where-its-parent-works)), `--description`, `--max-turns`, and trailing argv. The doorway omits `--ask`, `--account`, `--name`, `--budget`, `--worktree`, `--from-pr`, `--channel`, `--stdin`, `--continue`, placement flags, output and input formats, retries, and verification. Each stays reachable through `rimz agents`. The retired `--detach` flag still parses and refuses (`reject_detach`).

A launch prints the minted petname and returns. Its stderr receipt, shared with `rimz agents -p --bg` through `supervised::output::write_background_receipt`, names the fleet report, the agent-visible response path, and the `wait` command that checks on the run. `--wait` leaves the background run unchanged and passes the petname to the shared `agents_cmd::wait_agent` join, which returns at once. `--json` on a single launch requires `--wait`.

Launch resolution for the positional profile and the `profiles` catalog read `[subagents.profiles]`, while `rimz agents` reads `[agents.profiles]`. A positional profile named in the wrong section produces an error that names both. `--agent` on a subagent launch checks `[subagents.profiles]` first, then `[agents.profiles]`, then registered runtimes, dropping `subagents`, `idle-compact`, and `keep-warm` from any direct profile it inherits. Commands and teams are shared between the two; a team-role spec uses its role's profile from `[agents.profiles]`. The `profiles` catalog never lists teams, because one launch produces one agent.

### The parent caps a child's posture

The planner caps a launched child's posture at its parent's on two axes, isolation and permission mode, at launch and again at parent-message resume. The caps keep an agent tree consistent and are not containment. They judge only the posture RimZ plans: trailing argv is appended after the permission step (`plan::finalize_agent_cell`), so a provider switch such as Claude's `--dangerously-skip-permissions` reaches the child unchecked.

Isolation follows the [subagent cap](../sandbox.md#the-subagent-cap), which `sandbox.md` owns with the isolation levels. `plan::cap_child_isolation` reads the launching process's ambient isolation, so it needs no caller row.

Permission mode is capped in `crates/rimz/src/harness/plan.rs::cap_child_permission`, called from `crates/rimz/src/harness/plan.rs::plan_supervised_launch` before finalization renders argv. The ceiling is the caller's recorded `AgentState.mode`, never an environment value, with an unrecorded mode read as auto. Both the requested mode (including its default) and the ceiling map through the child kind's `crates/rimz/src/agents/definition.rs::LaunchPermissionArgs::resolve_mode` before ranking plan < ask < auto < yolo. A request at or below that ceiling passes without a clamp, and a launch with no resolved caller is not capped. Pi maps every mode to yolo, so a Pi child is never clamped or refused and emits no permission-clamp notice: RimZ cannot restrict a provider with no permission system.

| Source of a mode above the ceiling | Result |
| --- | --- |
| `--yolo` | Refused with `ChildPermissionErr::Explicit`, which names the parent's mode and the fix: a parent launched with `--yolo` or a `mode: yolo` definition. |
| A profile's `mode:` (`PermissionClampSource::ProfileDefault`), a mode cell such as `codex-yolo` (`ModeCell`), or the auto default (`AutoDefault`) | Lowered to the ceiling. The planner turns the default into an explicit choice, so finalization strips the profile's permission args, adds the ceiling's, and records the ceiling on the cell, the agent record, and the run record. `SupervisedLaunch.permission_clamp` carries the lowered mode and its source to the stderr note (`launcher::Notice::PermissionClamp`, rendered by `cli::launch_interaction`). |

Parent-message resume ([resuming an ended child](#resuming-an-ended-child)) rechecks the replayed mode with `crates/rimz/src/harness/plan.rs::check_child_permission_mode` against the parent's current recorded mode, mapping both through the child kind just as launch does. It refuses a higher mapped mode with `ChildPermissionErr::Resume` instead of lowering it. The fix it names is a new child. The same path reruns `plan::cap_child_isolation` on the child's recorded override and its re-read profile default.

A yolo child that resolves to scoped isolation gets a stderr note that `sudo` and setuid binaries do not work in the view, with `--isolation host` as the fix (`SupervisedLaunch.yolo_sandbox`, `launcher::Notice::YoloSandbox`, rendered by `cli::launch_interaction`). The note is skipped under a scoped parent, where the cap refuses that fix, and at parent-message resume. Why the view blocks escalation is in [sandbox.md](../sandbox.md#the-launch-plan).

Both caps apply only where the planner sees `subagent` set, and at parent-message resume. Peers (`rimz agents`, interactive or `-p`), teams, loop fires, workflow steps, restart, fork, and cohort resume are uncapped. A child's restart sets `subagent` on its exec request for child launch treatment, but replays the recorded mode and isolation without re-capping.

## Fanout

`rimz subagents fanout` reads a JSON task array from a file or stdin and desugars every entry through the same `into_agent_launch`. For each of `timeout`, `warn`, and `grace`, a task's value wins over the fanout flag; an unset value is left for supervised preparation, where the timeout takes the child definition's `timeout:` before the configured default. Fanout-level `--keep` applies to every task. Permission mode, isolation, wait, retention, and passthrough argv are not per-task fields, and the task format rejects unknown fields. Every task still passes the planner's [parent caps](#the-parent-caps-a-childs-posture).

The whole array is validated before the first side effect: parsing, a nonempty array, required fields, every `prompt_file` read, every task and flag deadline duration, and every task's `cwd` and `add_dirs` resolved against the room (`parse_fanout_launches`, then `resolve_launch_cwd` per path in `crates/rimz/src/cli/subagents/mod.rs::fanout_children`). A validation failure launches nothing. Pane opens then run one at a time in the caller process, so two backend splits never race against the same anchor pane. Each child process starts as soon as its pane opens, so the children run in parallel.

The supervised runner returns each child's minted petname and run id to the command. Fanout therefore collects identities directly instead of diffing store snapshots, which another launch in the same family could confuse.

| Mode | Prints |
| --- | --- |
| no `--wait` | each minted name as its pane opens, then the receipt. With `--json`, a map of name to `{run_id}` instead of names |
| `--wait`, text | each minted name as its pane opens, then through `wait_agent`, in launch order, a report row for each settled child and a still-running line for each other |
| `--wait --json` | a map keyed by minted name: a settled child's entry names its response file, a running child's is `running` with exit `75` |

Launch and fanout waits key their output by minted name, because no reference was typed. An explicit `subagents wait` keys by the caller's typed references while resolving them within the caller's children, and each summary entry carries the pet `name` separately.

A failure partway through aborts the remaining launches and names every child already started. A budget refusal exits with the budget-exceeded code. The started children are not rolled back: they keep the ordinary supervised lifecycle, and the error points at `rimz subagents wait` and `stop --all`.

## Caller policy

The supervised runner resolves the durable caller before it resolves a subagent profile. [`subagent_policy.rs`](../../../crates/rimz/src/harness/subagent_policy.rs) then resolves the caller's effective `subagents = [...]` list in launch chain order under its recorded `--agent` and kind: when set, the positional profile must appear literally in the list (any `--agent` override is not checked against it). Refusal happens before provider preflight, store append, or mux mutation.

`subagent_policy::catalog` is the one owner of catalog assembly. Both `rimz subagents profiles` and the parent's launch reminder read it ([below](#parents-learn-their-catalog)). It lists the `[subagents.profiles]` entries, the shared commands, and each runtime variant (such as `opencode-mini`) whose kind has a `[subagents.profiles]` base or that the allowlist names. It returns `NoneAllowed` for `subagents = []`, then filters by the literal allowlist and returns `Disabled` when nothing survives; `Available` is never empty. A caller whose profile sets no `subagents` list, or that resolves to no profile (a user shell included), sees the unfiltered catalog.

## The child works where its parent works

A child's checkout defaults to its parent's recorded checkout, not to the directory the parent's shell is in. The runner first resolves its workspace from `"."`, so a parent that writes a brief under `/tmp` and launches from there would hand the child `/tmp`. Right after ancestry, `anchor_caller_workspace` ([`launcher/checkout.rs`](../../../crates/rimz/src/launcher/checkout.rs)) resolves the workspace again from the caller's `AgentState::lane_path` (`checkout`, else `worktree_path`). Everything after reads that value: profile qualification, channel inference, `RIMZ_WORKTREE_PATH`, and `resolve_launch_checkout`. That checkout's cwd becomes the pane cwd, the provider cwd, the sidebar options, `RunRecord.worktree_path`, and the child's recorded checkout. When the path is a repository subdirectory selected by the parent's `--root`, it stays the child's `worktree_root`, and workspace resolution supplies only the surrounding metadata.

The parent's `worktree_path` is stamped from its launch cwd (or filled from a hook process's cwd) and does not follow a shell `cd`. Both resolutions share the room pin's project root, so the store and effective config opened before the re-anchor stay correct.

The same re-anchor applies to interactive peers, supervised peers, and teams launched by an agent. Loop fires have no agent caller and keep their resolved directory. A caller with no row or no recorded path keeps the invoking cwd and logs at debug. A recorded checkout that no longer exists refuses, naming the path, before any store append or mux action.

An explicit `--cwd` overrides the default after the parent checkout is validated. Entry resolution converts the path to an existing canonical host directory, and `resolve_launch_checkout` returns a user-owned checkout there. The pane, provider, channel (that directory's own lane), `RIMZ_WORKTREE_PATH`, and the durable child and run `worktree_path` follow that directory without changing the room pin, store, or effective config. Restart, fork, child resume, and rebirth reuse the recorded directory. RimZ takes no cleanup ownership of that directory.

Repeated `--add-dir` and fanout `add_dirs` resolve through the same caller-path translation, keeping canonical host paths in flag order without duplicates. `LaunchParams.add_dirs` and `untrusted_checkout` survive lifecycle projection and every relaunch carrier; the latter records the undecided-cwd fallback. Added directories join the scoped view's required paths. Claude unions them into `permissions.additionalDirectories`; Codex joins them to its selection-dependent room-root grant; Antigravity passes one `--add-dir` each, which agy opens as a workspace, loading its configuration and running its hooks, and the launch warns (`AddedDirs::Workspace`). Other kinds warn and launch without a grant. Saved-untrusted directories refuse before launch, including on re-entry; no added-directory path grants folder trust.

Explicit `--empty-dir` and fanout `empty_dir` widen the existing `prepare_empty_dir` placement to an explicit cwd and a profile without the key. They require placement, so `Decision::Fallback` refuses rather than printing a notice and continuing. The displaced cwd becomes `checkout`, keeping its lane; it is not automatically an added directory. Before empty-root preparation, the displaced target passes home-directory and Codex saved-untrusted admission. Profile-only placement retains its old eligibility and fallback behavior.

Before that placement, `prepare` checks an explicit cwd on a fresh Claude or Codex child without `--empty-dir`. Only a typed undecided refusal takes the fallback: require an empty start, append the target to `add_dirs`, and record `untrusted_checkout`. A restricted refusal is never converted. An unusable empty start combines its cause with the original trust fix before any run or pane exists. After provider preflight, the launcher emits an `UntrustedCwdFallback` notice; `open_fresh` appends the pending `untrusted_cwd_fallback` assist only after successful pane placement, consuming it before retaining retry state. The CLI renders `rimz subagents <profile> [--agent <value>] --cwd T --empty-dir --add-dir T "<brief>"`, keeping the caller's `--agent` value (fanout: the task's `agent` field) immediately after the profile and no other caller flags. The marker, not current provider trust, chooses the reminder's explanation on every re-entry. The explicit spelling has no fallback marker or assist. `agents show --json` exposes `checkout`, nonempty `add_dirs`, and a true `untrusted_checkout` on its agent entry.

Provider preflight then runs against the chosen checkout. Besides hook installation and trust, a child must pass the folder-trust preflight for every provider that models it, including Claude and [Codex](../agents/adapter_codex.md#directory-trust-preflight). An inherited undecided project or a saved-untrusted folder refuses with the fix. An explicit undecided Claude or Codex cwd instead takes the empty-directory fallback, so neither path leaves the child on a trust screen until its deadline. A Codex projectless folder without a saved decision passes.

## Pane zones

With an ambient pane, a subagent launch uses `RunPlacement::SubagentZone`, a placement reserved for this doorway and absent from the general placement flags and config. `select_subagent_zone_strategy` ([`launcher/placement/zone.rs`](../../../crates/rimz/src/launcher/placement/zone.rs)) picks the first strategy whose condition holds:

| Order | Condition | Strategy |
| --- | --- | --- |
| 1 | the launch channel's companion family is live | append to the first companion with room, or open the next numbered companion when all are full |
| 2 | the caller is a configured team member | open a new companion tab after the launcher's tab |
| 3 | a launched child of the caller is live beside the caller's pane (same session and view, tiled) | stack against the newest such child |
| 4 | the caller's pane is live | split right from the caller, beside its whole stack when it is a member, on either backend |
| 5 | none of the above | open a new companion tab |

Companion titles are `#<channel> subagents`, using the launch channel without a pane lookup. On the main channel they take `<view> subagents` from the caller's view, with status glyphs stripped, or `subagents` when the view cannot be resolved. An out-of-pane child opens a tab with the same base title.

Both backends keep one stack member expanded, and focus opens a member. Members of a team sharing one channel reuse that channel's companion family instead of following the newest child into an unrelated run tab. A solo split that fails without ambiguity also falls back to a companion tab, which rule 1 then reuses for later children.

Each companion tab holds one collapsing stack, founded when the second child arrives (`MuxBackend::append_companion_pane` in [`mux/mod.rs`](../../../crates/rimz/src/mux/mod.rs)). Before each append the backend reads the anchor's whole view, excluding sidebar chrome and floating panes. It accepts only a lone work pane or one stack, including retained and not-yet-bound terminals. The add targets the bottom work pane and must leave the expanded member at least 10 content rows. Companions are tried in numeric order, and `Full` guarantees no payload ran: a view without work panes, a grid or manual split, a zoomed tmux window, or a fullscreen or suppressed Zellij tab takes no new children. When every companion is full, the family grows into `#<channel> subagents 2`, then `3` (the same numbering applies on the main channel). Existing grids drain in place. On Zellij below 0.45, background stacked adds cannot preserve focus, so each child after the first opens its own numbered tab.

An append never balances or reads geometry after spawning, and never launches a successful spawn again. A definitive spawn error tries the next companion. An uncertain spawn (`Timeout` or `Output`) or a failed companion-tab response fails the launch instead of retrying the same durable run elsewhere. Zellij can silently drop an overfull stacked add while its payload keeps running unseen. The content-row floor leaves at least five rows of margin above that limit; a terminal shrink of five or more rows between the pre-spawn read and the add remains an accepted edge, with no post-spawn confirmation or retry. When the last child pane closes on its own, the companion's sidebar sees an empty view and closes the tab; closing that child's card with `card.close` takes the tab in the same step ([close and reopen](../../reference/cli/sidebar.md#close-and-reopen)).

Zone mutation is serialized per workspace by `lock_subagent_zone`, held across the mux open and the wrapper-bind wait. `split_into_subagent_zone` takes the parent explicitly from both callers (`launcher/placement.rs::execute_run` and `launcher/child_resume.rs`) rather than reading the calling process, so a reopen from a user shell or detached helper lands in the parent's zone. Anchor selection joins durable child ancestry and pane bindings with one authoritative mux pane list, so an ended `--keep` child stays an anchor while its pane is live and a dead pane never is. Companion discovery matches exact base or numbered view names after stripping sidebar status glyphs, grouped by the current mux view identity, and keeps views even when their children have not bound yet. If projection, caller resolution, the authoritative pane listing, or a companion append's pre-spawn preparation fails, the child opens a companion tab and records `subagent_zone_lookup_failed` in the [diagnostic log](../diagnostics.md). A pre-spawn append failure abandons the remaining anchors and opens the next numbered companion, with lookup `companion_stack`. A lookup failure before companion discovery can create a duplicate base name, and discovery keeps both views as anchors on later launches.

The wrapper records its pane binding asynchronously, so a subagent launch polls the durable rollup for up to `SUBAGENT_PANE_BIND_TIMEOUT` (3 seconds) before releasing the zone lock. Sequential fanout launches therefore see the preceding child as an anchor. A wrapper that does not bind in time does not fail its launch: the next team launch still finds the companion from mux truth, and a solo launch takes the no-anchor strategy. Launch and resume waits both record `subagent_pane_bind_timeout` in the [diagnostic log](../diagnostics.md), with the pane-open duration and the final read's error or launch-matching rows.

## Launch generations and parentage

Ancestry resolves in the supervised runner before layout compilation, provider preflight, worktree creation, store append, or any mux action, so a refusal leaves nothing behind.

Before opening the store, interactive and supervised launch entries check an explicit `--root` against the calling agent's verified environment pin. Different room ids refuse with `LaunchAncestryError::RoomMismatch`, which names both rooms and the caller and tells it to drop `--root` and use `--cwd`. The check creates no room directory. A matching room, an absent or invalid pin, or a human shell without agent identity leaves resolution unchanged, and non-launch commands keep cross-room `--root` access.

The caller resolver ([`harness/ancestry.rs`](../../../crates/rimz/src/harness/ancestry.rs)) finds the calling agent's durable row in three tiers:

| Tier | Evidence | Resolves to |
| --- | --- | --- |
| launch environment with a launch id | `CallerIdentity::from_env` | the launch's current row through `address::launch_row` (a live row by pane-owner order, otherwise the latest active one), with kind corroborating |
| launch environment without a launch id | the agent kind plus the ambient pane id | the single live, same-kind, non-provider-subagent row stamped with that pane. More than one match refuses |
| no launch environment | `from_process_ancestry` | the nearest ancestor whose PID matches a live agent `RuntimeOwner`, and whose process-start token matches when the owner recorded one, preferring the current pane when rows share an owner |

Resolving a launch id to its current occupant, instead of the first row that carries it, keeps a child off an ended predecessor after a compaction, a `/new`, or a fork. Kind corroboration stops a stale cross-provider environment from attaching a child to the wrong row. The second tier covers an agent process that was already running across an upgrade and so has no launch id.

`resolve_launch_ancestry` then reads the caller's durable `launch_depth` as a launch generation:

```text
no resolved caller (a human shell)              → no stamp
caller is a launched child                      → refuse (SubagentCaller)
generation = caller.launch_depth ?? 0

rimz agents / rimz teams:
  generation >= max_chain_length                → refuse (ChainExceeded)
  parent_agent_id = None
  launched_by = (caller.kind, caller.launch_id ?? caller.agent_id)
  launch_depth = generation + 1

rimz subagents:
  parent_agent_id = caller.launch_id ?? caller.agent_id
  parent_agent_kind = caller.kind
  launch_depth = generation + 1

rimz agents fork (does not call resolve_launch_ancestry):
  launch_depth = source.launch_depth          (never refused, caller ignored)
```

The stamp names the caller's launch, and falls back to its session id only for a caller without a launch id. A launch outlives its conversation rows: compaction, `/clear`, a follow-latest switch, and a forked retry each end the old row, and a link to one row would read the child as parentless while the parent is still working. The field keeps the wire name `launch_depth` so existing event logs replay.

`[agents] max-chain-length` ([`config/agents.rs`](../../../crates/rimz/src/config/agents.rs)) defaults to `3`. A human-started agent is generation 0. Three successive peer launches produce generations 1, 2, and 3, and the generation-3 agent cannot launch another peer. Subagent launches skip the chain check because a subagent cannot extend the chain. Every fanout entry gets the same parent and generation.

Two operations copy a generation instead of launching:

- A fork ([`cli/agents_cmd/fork.rs`](../../../crates/rimz/src/cli/agents_cmd/fork.rs)) copies its source's `launch_depth`, whether a user shell or another agent runs it, sets no `launched_by`, and is never refused by the chain cap. It records a child's [profile-table doorway](./fleet.md#posture) for later relaunches. A fork cannot restart a chain at generation 0, but the cap does not limit how many forks exist: an agent at the cap can fork itself, and so can each fork.
- `rimz agents molt` swaps the calling agent into a fresh session under the same launch and handle. It copies the recorded generation, launcher, and parent fields without resolving new ancestry, including at generation 3. A launched child cannot molt.

Ancestry failures are `LaunchAncestryError`, written for a reader that is itself an agent. Each message states the refusal, explains the limit, and ends with *do not retry this command*. The exception is `RoomMismatch`, which ends with the `--cwd` fix because that variation can succeed. An agent that reads "launch refused" without a terminal instruction tends to retry with a variation, so the phrasing is load-bearing.

## Children cannot delegate again

Four independent layers keep a child from launching work of its own. Each covers a gap the others leave.

The launch planner refuses any `rimz agents`, `rimz teams`, or `rimz subagents` launch whose durable caller is a launched child (`SubagentCaller` above). That blocks another RimZ process.

Every child request, fanout entries included and whatever its skills, carries a fixed body in its launch reminder. The body tells the child to do the assigned work itself and report the result in its final response, to send its caller no progress notes (each one stops the caller's [blocking wait](../../reference/cli/agents.md#wait)), and to end its turn with the question when it needs its caller's answer, and it names no delegation skill. A child with a [result file](#response-files) gets a body naming that file as it sees it in place of the final-response instruction (`crates/rimz/src/harness/launch_reminders.rs::result_file_reminder_body`). It travels the adapter's [launch reminder channel](./fleet.md#launch-reminders) with the child's model line, which the launch profile's `model-reminder` can turn off. An adapter with no append-system-text channel, and an Antigravity child with no prompt sources (its channel is the agent file such sources plan), gets the body alone, as a tag-wrapped suffix on the user prompt (`supervised_prompt` in [`launcher/resolve.rs`](../../../crates/rimz/src/launcher/resolve.rs)). fleet.md owns which adapters have which channel, and [adapter_codex.md](../agents/adapter_codex.md) owns the Codex `developer_instructions` route.

The child's skill view also withholds model invocation of `rimz-subagents`, `rimz-agents`, and `rimz-teams`, even when its profile lists them or has no list, and its MCP server is not handed those capabilities, whose every launch would refuse (`crates/rimz/src/harness/launch_reminders.rs::LaunchReminders::denied_capabilities`, which denies a workflow judge the same three and a profile with `subagents = []` only `rimz-subagents`), beside the skills a launch withholds because it serves their MCP tools, which a host Antigravity launch does not ([host mode](../sandbox.md#host-mode)). Host Claude and Codex use their provider switches; a supported scoped view rewrites only the withheld skills when no list is configured. Other skills retain the profile's policy. The profile's own list still decides refusals and unenforced-list warnings, so the exclusion introduces neither for an unconfigured child ([skill views](../sandbox.md#profile-skill-views)).

### Native delegation policy

Where the provider exposes a verified native restriction, the exec compiler disables its delegation tool for every child, and for any other launch with an `Available` catalog and the callable `rimz-subagents` skill or the served `subagent` tool. Callability includes the applied tools list as well as the skills list ([launch reminders](./fleet.md#launch-reminders)). The predicate lives beside the catalog offer in `crates/rimz/src/harness/launch_reminders.rs::LaunchReminders::disable_native_subagents`; the argv and environment seams and the agent-file plan read it after profile inputs are applied:

| Provider | Process restriction |
| --- | --- |
| Claude | merges the profile's disallowed tools into one final `--disallowedTools` occurrence that denies `Agent` |
| Codex | removes earlier `agents.enabled` and `features.multi_agent_v2` config overrides and every split or equals `--enable multi_agent_v2`, then appends `-c agents.enabled=false -c features.multi_agent_v2=false`; `features.multi_agent` is untouched |
| OpenCode | appends `"-opencode.tool.subagent"` last in `OPENCODE_CONFIG_CONTENT.plugins`, keeping every other key and leaving permissions untouched |
| Pi | no built-in subagents; third-party extensions stay installed because disabling them all would remove unrelated tools |
| Antigravity | a fresh launch's agent file leaves out `invoke_subagent`, `define_subagent`, `manage_subagents`, and `run_workflow`; a resume, or a launch with no prompt sources, writes no file and keeps native delegation ([the agent file](../agents/adapter_antigravity.md#the-agent-file)) |
| Other adapters | no native-tool restriction until one is verified |

OpenCode removes the built-in tool, so neither an agent-specific permission allow nor `--auto` restores it. The base object is composed launch env over inherited login env. Malformed content is replaced with the selector-only object and produces one launch warning with the problem and fix ([adapter_opencode.md](../agents/adapter_opencode.md)).

The same rule applies on launch, resume, and fork, including peers and team members. An explicit `Agent` in a definition's `tools:` loses to it only when the rule applies. A parent with neither the callable `rimz-subagents` skill nor the served `subagent` tool, with a disabled catalog, or without a catalog keeps its native delegation tools. Workflow judges (a Judge-role pin) get no catalog and keep native delegation, since a `rimz subagents` child would get a writable checkout. Preflight and managed process compiles carry no catalog; recovery without effective config also keeps native delegation and warns about default reminders.

### Parents learn their catalog

A non-child launch other than a workflow judge's gets the other side of that body, the subagent catalog its profile allows, when the `rimz-subagents` skill is callable or the launch serves the `subagent` tool ([fleet.md](./fleet.md#launch-reminders)). The exec wrapper builds it through `subagent_policy::catalog` on every fresh launch, resume, fork, restart, and recovery. It points the agent through `mcp(subagent)` where the launch serves it, else through `skill(rimz-subagents)` (`crates/rimz/src/harness/launch_reminders.rs::resolve_reach`), defines `subagent(x)` as the profile `x` in the list, and lists the same filtered profiles as `rimz subagents profiles --json`. A profile with no configured entries, or no loaded entries matching its allowlist, gets `SubagentCatalog::Disabled`, a body that says no subagent profile is configured for it and tells it to work directly, without claiming its profile refuses launches. A profile with `subagents = []` gets `SubagentCatalog::NoneAllowed` and no Subagents paragraph: the launch is neither served the `subagent` tool nor offered the `rimz-subagents` skill (`crates/rimz/src/harness/launch_reminders.rs::LaunchReminders::no_subagents`). Catalog presentation does not change admission: `check_allowed` still permits bare kinds for a caller with no `subagents` list. Only adapters with an append-system-text channel receive it, because an interactive launch has no user prompt to extend safely. If effective config fails to load during crash recovery, the wrapper warns and falls back to default reminders instead of killing the recovered pane: the catalog and team paragraphs drop, and the model line stays ([fleet.md](./fleet.md#launch-reminders)).

## Who counts as a launched child

Agent-scoped `list`, `wait`, and `stop` select through [`address::launched_children`](../../../crates/rimz/src/address.rs). It keeps rows where `is_launched_child` holds and `parent_is` accepts some row of the caller's launch, then sorts by registration. Widening the parent to its whole launch keeps the family together when a launched parent adopts its provider session id, and when a child stamped with one conversation's session id must list under the successor that replaced it. Because a child cannot launch again, the set is exactly the caller's direct launched children: peers carry no parent, and provider-native children carry no generation.

A session-id link resolves only where the row it names is visible, which costs the sidebar one case. The harness verbs, the watchdog's seed and confirmation helper, and the orphan scan read `RuntimeScope::Audit`, which keeps ended rows, so they reach the parent through an ended predecessor. The sidebar reads `RuntimeScope::Runtime`, which hides ended rows, so a child holding a session-id link renders promoted to its own card once that conversation ends. Callers with a launch id never produce such links.

| Verb | Caller | Projection | Selects |
| --- | --- | --- | --- |
| `list` | resolved agent, no `parent` term | published room snapshot's selector rows, plus `RuntimeScope::Audit` closed rows | the caller's children, ended ones included, so completed work stays listable |
| `wait` | resolved agent | `RuntimeScope::Audit` | the caller's children, ended ones included, so completed work stays joinable |
| `list` | unresolved (user shell), or a `parent` term | published room snapshot's selector rows, plus `RuntimeScope::Audit` closed rows | launched children in the current channel, or every channel when there is none |
| `stop` | resolved agent | alive snapshot, then `ended_at.is_none()` for `--all` | the caller's live children |
| `wait`, `stop` | unresolved | none | refuse, since they need a durable caller |

`list` is a preset over the [agent selector](../../../crates/rimz/src/address/selector.rs) `rimz agents list` reads (`preset_rows` in [`cli/subagents/mod.rs`](../../../crates/rimz/src/cli/subagents/mod.rs)): `selector::live_rows` over the published snapshot, behind the same `room::require_live_mux` check, then `selector::closed_rows` over the Audit projection for children the snapshot no longer carries. It keeps launched children only, then the caller's set: the rows `address::launched_children` names for a resolved agent, else the current channel's rows, unless a term names `lane`. `Query::matches` then applies the `--filter` terms without the `live` and `parent` defaults `Query::admits` adds for `agents list`. For a user shell, `address::agent_in_lane` keeps every launched child that matches `Ctx::channel`. A root-lane child matches `main`. Any other child matches by its lane's name, exactly or as the dashed form of a branch-style filter, and a child with no lane matches no channel. Membership never comes from a live parent, so ended parents and adopted identities do not hide child history. [`address::launched_parent`](../../../crates/rimz/src/address.rs) is the inverse join: each row's `PARENT` cell names its parent launch's current row. A plain shell in the project directory cannot derive an in-place team's channel, named after the team, from cwd, so `Ctx::channel` is `None` there and `list` shows every channel, with the channel column telling in-place lanes apart. A RimZ-launched pane carries `RIMZ_CHANNEL`, and a shell in a separate worktree derives its worktree channel.

### What `list` prints

An agent caller's table has `SUBAGENT`, `KIND`, `AGENT`, `RUN`, and `ELAPSED` columns. A user shell's table adds `PARENT` and `CHANNEL`, and so does an agent caller's whose filter names `parent` (`list_scope`). Rows sort by their newest run: open runs first, then settled ones, newest answer start first within each, and children with no run last (`child_reports`).

The `AGENT` column prints the selector row's status, `card_status`, so a child reads what `rimz agents list` shows for it. A child with a nested card under its parent's row takes that card's status: silent past the stall window reads `failed` with a `silent <age>` label, and an ended child reads its settled status. Otherwise the row reads `AgentState::rowless_status` with the room's [reset-window and exhausted-resume demotion](../agents/model.md#the-sidebar-ladder), the same ladder the nested card applies to an unended launched child:

- a pausing marker reads `paused` while its park can resume, and `failed` once spent
- a fatal marker reads `failed`
- both apply on a raw-`failed` live row too

The demotion reads durable provider capacity and park records over the published snapshot's agents, with its terminal resume outcomes, as `agents list` computes it. `RUN` is the newest run's own status, so a paused or spent park can sit beside `running`. `--json` keeps the displayed marker as `turn_error` beside the row's flat keys, and the muted line takes the nested card's label, else the displayed marker's label (or class words), else the activity line.

### What `wait` joins

`wait` requires at least one name. Given none, it fails and prints the caller's child list, so a copied command never joins an older fleet. `--any` exits by the first named child that has settled. Names resolve against the child set only, so an address naming another agent in the room fails with ``not one of this agent's subagents``. Each child's newest run is the run of the child's kind with the latest `started_at` whose `agent_id` or `agent_name` matches the child (`newest_run_for_child`). The two matches are alternatives, so a newer run under the same name wins even when its session id differs.

A parked run has no answer yet, so `wait` prints a paused child's still-running line and exits `75`; its answer reaches the parent in the report once the run settles.

## The lifecycle, end to end

Each step names the section that owns it.

1. Launch. Caller policy and ancestry pass, the run record and pane are created, and the petname prints. With `--wait`, the parent then joins.
2. Startup relaunch. A provider that dies before RimZ accepts its first lifecycle observation, other than a configuration refusal (exit 78), is relaunched in the same pane, up to `[agents] startup-relaunches` times. Handle, run, pane, and deadline stay the same, and a relaunched death is never reported ([fleet.md § A startup death is relaunched first](./fleet.md#a-startup-death-is-relaunched-first)).
3. Work. The child runs until its work ends. A clean turn end with an owed harness wake [parks](./scripting.md#parked-runs) the run as `Running`, so the parent's digest still waits for it. An interrupted turn [holds the run open](./scripting.md#interrupted-subagent-turns) for 60 seconds, and a new turn in that time continues it. Otherwise the child's hooks fold a terminal status into the run record.
4. Publish. The newly terminal write publishes the answer to a [response file](#response-files) before the record lands.
5. Report. The child's in-pane wrapper runs the [fleet reporter](#the-fleet-report). Once nothing in the fleet is still running, the reporter queues one digest to the launcher.
6. Clean up. After its report is queued or joined and the child rests unfocused for `[agents.subagents] linger` (default `10m`), the wrapper stops the idle child, stamps its row's `ended_at`, and closes its pane ([cleanup after receipt](#cleanup-after-receipt)). One-shot providers exit on their own. Interactive providers stay available for [follow-ups](#follow-ups-and-resume) until then.
7. Retain. The run record survives the close. The ended child stays under any visible row of its parent's launch, so `list` and `wait` still report the outcome. Every retained launched child (finished, stopped, timed out, or reaped) stays on the card with its rest status, behind the section's `▸ +N earlier` line once it finishes and from the parent's next user prompt on, and remains reopenable through `rimz agents resume` when its recorded session can resume ([sidebar.md § Sub-agent lists](../sidebar/sidebar.md#sub-agent-lists)). A retained child is at rest and never holds its parent in `running` ([model.md](../agents/model.md#observed-ends-and-reaped-ends)). The run record's own outcome is what `list`, `wait`, and the digest report.

A parent that is gone ends the child through the [backstops](#backstops), whatever step it is at.

A failed child's diagnostic tail is the launch error or launch-step refusal stored directly on the run, or else the pane capture that [scripting.md § Completion](./scripting.md#completion-folding-lifecycle-observations) sizes and caps. A pane capture reaches into scrollback, so a collapsed stack member keeps earlier output rather than only its visible row. Capture happens only after the last startup attempt and is not fenced to it, so recent output from earlier attempts can appear too.

## Response files

`harness::run::update_record` publishes the answer under the workspace lock before it writes a newly terminal record, so any reader that sees the terminal record finds the file too (`wait` loads the record unlocked). It publishes for a subagent, a peer turn, a team run, and a run whose `report_to` is `nobody`.

`run::publish_response` writes the non-empty `last_message` bytes, adding a trailing newline when missing. For a subagent that is the whole answer, every [parked turn](./scripting.md#parked-runs) included. Identical bytes leave the file untouched, an empty or absent message removes only that answer's file, and earlier answers are never rewritten. A publish failure warns without failing the transition.

A result-file run answers from a file instead of its final message. `crates/rimz/src/launcher/execute.rs::open_fresh` gives a subagent launch one when its cell can write (not `Plan` mode, and no `tools` list or one naming `Write`) and its launch text reaches it (`crates/rimz/src/launcher/resolve.rs::launch_text_delivered`, the check `supervised_prompt` also reads): `RunRecord::result_file` is `<agent_name>.output` in the temp unit `launch_plan::temp_owner` resolves at compile (the reader's for a launched child), and a stale file there is removed before the pane opens. `ExecRequest::result_file` carries it from the record through `fresh_agent_argv` and `launcher::swap::with_run`, so a resume, restart, or molt names the same file, and `launch_plan::compile` spells it through the child's `TmpView` for the launch text. `set_turn_message` and `harvest_last_message` skip such a run, so its `last_message` stays empty while it runs. The newly terminal write reads the file into `last_message` before the publish (`crates/rimz/src/harness/run/result_file.rs::read`: opened `O_NOFOLLOW | O_NONBLOCK` because the child controls the path and the read holds the workspace lock, regular asked of the open handle, lossy UTF-8 cut to at most 1 MiB of text with a closing line when cut, blank read as none) and removes it once the record lands. A failure to read or remove warns and never fails the settle. A record without `result_file` stays a final-message run to its end.

Every file sits under `out/<reader>/` (`StatePaths::out_reader_dir`), where `RunRecord.reader` is the launcher's handle stored at creation, or the run's own name when none was stored. A run without `agent_name` has no response path.

| Run | File (`run::response_path`) |
| --- | --- |
| subagent, first answer | `<agent_name>.output` |
| subagent, answer to follow-up N | `<agent_name>.<N + 1>.output` (`run::earlier_response_path` builds the same path for an earlier ordinal) |
| peer turn or team run | `<agent_name>.<run_id>.output`, so a later turn cannot overwrite an unreported answer |

Every path is the host path. Reading a response file does not count as a join, and response files die with the room: the run record stays the truth.

## The fleet report

A launcher's fleet is its launched children plus the peers it stamped with `launched_by`. When the fleet settles, the launcher gets one parked harness message listing every answer it is owed: the digest.

### Which runs a fleet owes

`harness::fleet::FleetRuns` is the one settlement rule. The reporter, its orphan-sweep backstop, the owed-wake fleet term ([parked runs](./scripting.md#parked-runs)), and the pending-wait projection all read it. For each member it selects:

- its newest run that is not a peer turn, matched by the launch id the run was stamped with, else by kind and either session id or a present name (`RunRecord::matches_agent`)
- every peer turn (`RunRecord.peer`) that is open or still owes a report, matched by kind and launch or bound session identity, never by name

It then drops each selected run whose `report_to` is `nobody`, and deduplicates by run id. A supervised parent parks at a clean end while any selected run is live or owes an answer's report, and the queued digest then holds it until delivery opens its next turn.

A selected run is held when it is open with a stored `limit_hold` ([a child parked on a provider limit](#a-child-parked-on-a-provider-limit)). `run::claim_limit_hold` writes the hold; the child's next turn or a newer pausing stop, a limit or an overload (`run::end_limit_hold`) and a reopen in the lifecycle fold clear it, and a terminal write keeps it, which `FleetRuns` ignores. The digest gate (`FleetRuns::any_working`) and the orphan sweep's fleet branch pass over a held run; the owed-wake fleet term, the sweep's parked-peer branch, and `FleetRuns::unsettled` still count it as open, so a parent whose only open child is held stays owed and parked.

`RunRecord.report_to` is the policy of the answer that the launch prompt opens: `launcher`, or `nobody`. Its producers are `rimz agents` and `rimz teams` with `--detach`, and workflow agent steps (`launcher/workflow_host.rs`). A subagent record carries `nobody` only when the retired `rimz subagents --detach` wrote it. Two source rules carry the policy, so no consumer needs its own guard:

- `RunRecord::answer_claims` never marks an answer on a `nobody` run as owed, so no digest row is built from it.
- `FleetRuns::of` drops a `nobody` run after the newest-run pick, so an older attached run of the same member does not resurface.

The digest, its backstop, the owed-wake fleet term, and the pending-wait projection therefore ignore detached work, running or settled, and a mixed fleet reports its attached members as soon as they settle. Lineage, the response file, the card, and an explicit join are unaffected. Because no reporter sees a `nobody` run, its newly terminal write is its one publication attempt: a failure warns, and `rimz agents wait` or `subagents wait` still reads the answer from the record. Reopening a detached subagent sets the record back to `launcher` without carrying the detached answer into `earlier_answers`, so only the follow-up is reported. A message that lands while the detached answer is still open is part of that answer. The receipt and the launch reminder make no report promise a detached launch would break.

### Composing the digest

The reporter is `report_settled_child` in [`cli/agents_cmd/subagent_report.rs`](../../../crates/rimz/src/cli/agents_cmd/subagent_report.rs). The child's wrapper calls it for a terminal record that owes any answer, and again on its exit path after guaranteeing a terminal run.

1. Read the launcher's current row (`AgentState::launcher`) from the audit projection. A missing or ended launcher queues nothing.
2. Publish a response file for every terminal run `FleetRuns` selects, before checking siblings. This repairs a failed transition publish and covers agent-launched `rimz agents -p --bg` peers, whose waits can return before the wrapper reports. Any selected run that is still live and not held then stops the reporter, and the last settler reports.
3. Collect every owed answer (`RunRecord::answer_claims`; an answer is owed when it has neither `report_message_id` nor `joined_at`, and a held run, being open, owes none), earlier answers first, then the current one, and measure each published file. Earlier answers keep metadata, not text: a missing earlier file renders as `no response`, and only the current answer is republished. Any other publish or measurement failure aborts before any stamp, so the backstop can retry.
4. Mint the message id and stamp it on every listed answer as `report_message_id`, through `run::report::record_report_messages`. The stamp is all or nothing under the workspace lock, and backs off if any selected answer was already reported or joined, has disappeared, or its run reopened.
5. Queue the digest, gate `Done`. A queue failure clears exactly those stamps so the backstop can retry.
6. Recheck: when every listed answer was joined while the digest was queued (`run::report::digest_fully_joined`), cancel it. Otherwise make one boundary delivery attempt to the launcher's pane. Immediate delivery is latency over the durable records.

When every row is a subagent run, the notice is `SubagentReport` (`SUBAGENT_REPORT`). Otherwise it is `AgentReport` (`AGENT_REPORT`), mixed fleets included. Both read `From: @rimz`.

The stamp-then-queue order makes the report race-safe. A fleet is every child launched before the digest is composed: a child launched while siblings run joins it, and one launched after composition starts belongs to the next. Two last settlers racing on the same fleet produce one digest, because the loser's stamp backs off and it queues nothing.

The digest text (`compose_digest`) has a heading, one row per answer, and one line per held child:

- The heading is `Your subagent settled:` or `All N subagents settled:`, with `background agent` in place of `subagent` unless every row is a subagent run. N counts rows, and several rows may name the same child or peer. With a held line it reads `N of M subagents settled:`, M counting rows and held lines. With two or more response files the heading appends `, responses total {label}` before its colon (`All N subagents settled, responses total {label}:`), over their summed `FileSummary`.
- A row carries the answer's status, the elapsed time from that answer's start (`after` for a timed-out answer, `in` otherwise), a reason for a non-completed answer, its task, and its response path with `FileSummary::label` (estimated tokens and physical lines from `FileSummary::measure`), or `no response`. A timed-out answer's path is labelled `partial response`. The reason is the last nonblank line of the failure tail, except for an answer carrying `limit_failure`, whose reason `limit_reason` computes at compose time from the cause, its `refill_at`, and the account a resume would use, as `harness::resume::resume_login` answers it (`ResumeLogin`: the same account, a changed one with its key, or one that cannot read the session), with the changed account's refill from `park_notice::refill_ahead` and the same account's from `refill_at`. The room's accounts are read once per digest, and only when a row carries `limit_failure`; a read failure counts as the same account.
- The task of a launch answer is the child card's description (which `--description` seeds), else a bounded first-line preview of the prompt. A follow-up uses the bounded first line of its own stored prompt, and omits `task:` when that is unknown. A peer turn always uses its own prompt preview.
- A held line (`compose_held_line`) names the hold's `until` as a wait and a local clock, or says the limit has refilled once `until` has passed. Its label is the child's displayed turn error, which the audit projection lacks, so the reporter first attaches the stored context (`store::agent_context::attach_child_display_context`) to every child with an open held run, whatever its raw status. The line repeats in every digest composed while the child is held; nothing stamps it.

The park-notice helper runs the reporter after a successful hold claim, so a sibling answer the held child was holding back goes out at once.

The digest ends at its last row, with no trailing instruction.

## Peer turns

An agent that launches a peer with `rimz agents` and a prompt is owed that peer's answer too, so each launcher-opened turn of a persistent peer gets its own `RunRecord` with `peer` set (`PeerRun`) and `subagent = false`. It records the peer's `launch_id` and, in `opened_by`, the launcher message ids that opened it (empty for a launch prompt). Terminal peer records never reopen, and `RunRecord::prompt_origin` classifies their prompt as parent-authored. Their response files use the run id ([response files](#response-files)).

[`harness/run/peer.rs`](../../../crates/rimz/src/harness/run/peer.rs) owns the interface:

| Function | Does |
| --- | --- |
| `create_peer_prompt` | prepares a pending run under the workspace lock, stamping `report_to` on a record it creates. A reused open run is returned unchanged, so the receipt describes the record rather than the flag. A team seat's launch prompt goes to `harness/run/team.rs` instead |
| `open_peer_run` | finds a nonterminal run by the peer's stable launch id |
| `record_run_delivery` | runs inside the [acknowledgment callback](./messaging.md#confirmation-and-retry) and enrolls a launcher's message into the peer's open turn, or [stamps a subagent's follow-up](#a-follow-up-reopens-the-run) |
| `fail_peer_run` | settles an open run and wakes its waiter |

Enrollment takes only a prompt-correlated confirmation (`DeliveryAckMatch::PromptCorrelated`), and only the messages the peer's `launched_by` agent sent. A peer has at most one open run, enforced at creation and enrollment. Further launcher messages join that run, including while it is parked, keeping its original task and adding opener ids. A turn that a later launcher message opens is a new record and reports to the launcher. Human-launched agents, provider-native agents, and team seats (`AgentState::is_team_seat`) are excluded before any run is scanned. The interface never puts a run id into the persistent pane's environment.

Interactive launch calls `create_peer_prompt` after `begin_agent_launch_batch` assigns the prompt leader's identity and before placement opens a pane. Only an agent-launched, nonempty prompt gets a pending run, and resume bypasses it. A compilation or placement failure fails the run. Both launch and message enrollment require the adapter's native root turn-start and turn-end coverage: an unsupported adapter still launches, but its receipt says explicitly that no report comes. A supported launch receipt maps the response path through the launcher's `TmpView` and names the wait and follow-up commands by the peer's handle, `@<name>#<channel>`, which resolves the pending run before registration.

A peer turn settles without closing its pane. Restart fails an open peer turn before it opens the replacement, and room rebirth fails open peer turns whether their peers recover or not. An idle peer with no open turn produces no failed result. The orphan sweep settles a turn whose peer died or is parked with nothing owed ([backstops](#the-orphan-sweep)).

An agent-launched team reports once per `Done` or cohort death instead ([team reports](./teams.md#team-reports)). `address::launched_fleet` leaves team seats out of the per-member digest.

## Joins, stops, and the digest

Inline joins, parent stops, and the digest share a two-field handshake per answer on the run record ([`harness/run/report.rs`](../../../crates/rimz/src/harness/run/report.rs)). Both stamps take the workspace lock and are first-writer-wins.

| Actor | Stamps | Then |
| --- | --- | --- |
| a join that prints a terminal run's answer or report row to its launcher during an open turn, or its answer to a person | `joined_at` on the printed answer's ordinal only | cancels the queued digest if every answer carrying its id is joined |
| a blocking `rimz agents -p` driver, before it closes the pane of a terminal run | `joined_at` on the printed answer only | same check |
| the parent's `rimz subagents stop` | `joined_at` on every answer of each selected child's newest run, before cancelling any run | checks each linked digest, then cancels the runs |
| the fleet reporter | `report_message_id` on every listed answer, before queueing | rechecks the digest after queueing |

A `joined_at` stamp excludes that answer from a digest not yet composed. Joining the current answer leaves unprinted earlier answers owed. A digest already queued is cancelled only when every answer it lists is joined, including answers carried through a reopen, so an unread row keeps the notice. A digest already delivered cannot be recalled.

A join whose every target is a run that reports to its launcher, run by that launcher while its row is live, defers to the digest (`crates/rimz/src/harness/run/report.rs::defers_to_report`, which shares `may_claim`'s launcher test and needs no open turn). `crates/rimz/src/cli/agents_cmd/wait.rs::wait_deferred` polls the set once: a settled run gets its response file published and its row from `compose_digest_row`, then the `may_claim` stamp, and an unsettled run gets a still-running line and exit `75` with nothing stamped, so its answer stays owed to the digest. Any other join, a mixed one included, blocks.

`stop` stamps its whole selection first because the first run cancellation wakes a reporter that would otherwise list an unstamped sibling. This also lets `stop` cancel several queued digests for a `--keep` child once every listed answer is joined or stopped. The wrapper's report trigger and its receipt gate both consult `RunRecord::owes_report`, so joining only the latest answer cannot hide an earlier owed answer from cleanup.

`wait` never closes a pane. The blocking join (a person's, or one on a detached run, a run another agent launched, or an interactive turn) prints a failed run's transcript tail and is the path for a caller that needs the run's text synchronously. The deferring join prints rows only, and its republished response file is how a launcher rereads durable history.

### Attendance

A printed result counts as consumed only while the calling agent holds an open provider turn. The [joined mark](../../../crates/rimz/src/launcher/agent_join.rs) resolves the caller against `Store::snapshot_cached()` and asks `AgentState::holds_open_turn`. The cached snapshot attaches provider rest certificates, so a turn settled without a `Stop` hook reads as closed, while native waits and proposed plans keep the turn open. Attendance is read again at each print: a background wait that outlives its parent's turn still prints, but leaves its rows unjoined so the digest reaches the parent at its next boundary.

An unresolved caller is a human shell and counts as attended. A failed snapshot read logs a warning and also counts as attended. `stop` skips the check because dismissal is deliberate.

## Cleanup after receipt

A launched child keeps its pane open after settling until its report is durable and the child has rested unfocused for `[agents.subagents] linger` (default `10m`, floor `1m`, read from machine config on each check so edits reach lingering children). `RunMonitor` in [`cli/agents_cmd/exec/supervisor.rs`](../../../crates/rimz/src/cli/agents_cmd/exec/supervisor.rs) polls the run record every 250 ms with self-cleanup on, and runs two kinds of `supervise-duty` helper (`cli/agents_cmd/exec/duty.rs`) so the wrapper itself never folds the store:

- A resident `Strand` helper runs only while the run is parked and nonterminal. It repairs the fleet's digests and checks for a [stranded park](./scripting.md#parked-runs) every 5 s. When the run leaves that phase, it finishes its current work and is reaped, never killed mid-write.
- A one-shot `Receipt` helper runs the [fleet reporter](#composing-the-digest) when the record changed since its last start and still owes a report, then answers whether the child is ready for idle close. A new record revision, queue `(mtime, len)` changes, a waiter leaving, a turn opening after an untaken follow-up, or a 60 s fallback trigger it, with at most one in flight and a 1 s floor between starts.

Helpers also finish their current work when the wrapper exits, and a strand helper stops iterating once its wrapper is gone.

Idle close requires all of these, in a receipt answer requested after the waiter left:

- the run is terminal, with no answer still owed a report
- no waiter is live on the run
- the current answer was joined (`joined_at`), or its run settled (`completed_at`) before this wrapper bound, or the report message it names is present in the queue or in any retained message-history bucket (closing the stamp-before-queue window without waiting for delivery). A follow-up's reopen clears `completed_at`, so a new answer always settles after the bind
- the child has no open turn
- no non-terminal queued message is addressed to the child
- the child's pane is not the focused pane by a fresh focus fact (`crates/rimz/src/sidebar/cache.rs::fresh_pane_focused`, within `FOCUS_HOLD_TTL` or a fresh focus anchor; missing, unreadable, or stale frames hold nothing)
- the child has rested for at least `[agents.subagents] linger` (falling back to the `10m` default when machine config is unreadable or invalid) from `crates/rimz/src/harness/idle_stop.rs::rested_since`, taking the latest of `turn_ended_at`, the settle marker, `last_activity` in `snapshot_cached`, and the wrapper's own bind timestamp (`bound_at`). A message-driven resume whose follow-up settled without opening a turn (`Duty::Receipt { skip_linger: true, .. }`) omits only this wait until a turn opens on that wrapper, while keeping every condition above.

`RunExecContext::idle_close_ready_with_clock` in the helper reads the queue before `snapshot_cached`, whose rest certificates inform `holds_open_turn`, and takes one clock instant after those reads to evaluate focus freshness and `linger`. The parent's turn state and the report's delivery are not read. When a store or queue read fails, the duty exits non-zero and leaves the pane open for the next check.

A `nobody` run owes no report, so `RunMonitor::poll` cleans it up as soon as no waiter is live and any [resume hold](#the-receiving-turn-hold) has released, without running the receipt duty. Timeout pane retention and the resume exit policy still read `keep`.

### `--keep`

`--keep` turns automatic completion cleanup off for every run state, so `RunMonitor` neither waits for receipt nor closes the pane. After provider exit the wrapper holds the pane only under a live parent watch. It restores the saved terminal modes, then prints `rimz: subagent {status}; --keep holds this pane until its parent ends; press Ctrl-C to close it`. Ctrl-C or the parent's end leaves the settled answer untouched, stamps `rimz.agent-ended`, retires the session's deliveries, and closes the pane. A SIGHUP or SIGTERM during the hold exits without a stamp or a pane close, and the [stop seam](./scripting.md#reclaiming-the-run-pane) finishes. Without a live watch, a kept child settles and closes instead of holding.

The parked Unix wrapper never folds the store while the provider lives (a scoped park holds only its drainer lease's path-only store handle) and has separate [private-memory budgets outside and inside huge pages](../performance.md#overhead-at-fleet-scale), with executable text shared across wrappers. A parked nonterminal run adds one resident store-owning strand helper until its owed wake lands. A `--keep` child's settled hold opens a store and is outside that budget ([the exec wrapper](./fleet.md#resident-wrapper)).

## Backstops

The wrapper path above misses when a parent dies, a wrapper dies, or a child stops making progress. Four mechanisms cover those cases, and none of them depends on `keep` or on the report policy: kept children and children reporting to `nobody` are covered too, so no launched child outlives its parent's launch. Stopping the parent through `rimz agents stop`, or `rimz teams stop` reaching that parent, also stops its live launched children first, kept ones included ([fleet.md](./fleet.md#stopping-an-agent-tree)).

| Mechanism | Runs in | Catches |
| --- | --- | --- |
| [parent watchdog](#the-parent-watchdog) | each child's wrapper | the parent's launch ended or its pane is gone |
| [orphan sweep](#the-orphan-sweep) | the room host, once a minute | a missed digest, an abandoned peer turn, a child whose parent is gone and whose watchdog missed it |
| [deadline ladder](#the-deadline-ladder) | the room host's heavy-lane tick | a pending or running child past its timeout |
| [limit holds and failures, and stall notices](#a-child-parked-on-a-provider-limit) | the room host's heavy-lane tick | a child paused on a provider limit, or silent or stopped past the stall window |

The last three run in the room host, so without a sidebar host there is no sweep, deadline enforcement, limit hold or failure, or stall notice.

### The parent watchdog

The watchdog ([`harness/parent_watch.rs`](../../../crates/rimz/src/harness/parent_watch.rs)) runs on its own thread in every subagent wrapper with a run context and launch identity, and continues through a post-exit hold. Admission is decided once at wrapper start, even if its startup projection cannot resolve the parent. An unseeded watch, or one whose seed reads the parent's agent ended (`WatchdogSeed::parent_ended`, judged over the holder's rows as `agents::state::agent_end` judges them), asks its first confirmation at once. Its seed cursor precedes the startup projection read and is captured under the rotation lock.

Every `PROBE_INTERVAL` (60 seconds), `LaunchTail` streams lifecycle, launch, and attach frames into the parent launch's member map without opening a store. A frame stamped with the parent's launch id joins the map whatever its kind, so a cross-kind molt's replacement is a member, and a `failed` launch frame counts its provisional row as ended. Attach frames can move the parent pane but do not revive ended members. Three things nominate a verdict:

- every known member has ended, or one member ended in the tail while another stays live (which of them holds the agent is the helper's to read)
- a tail error or an archive gap
- `PANE_GONE_STRIKES` (3) distinct `RequireAuthoritative` mux reads that lack the pane. A cached roster hit counts as present

A nomination asks a one-shot `supervise-duty ParentProbe` helper, which resolves the current launch against the audit projection and answers ended only when the agent has (`agents::state::agent_end`: every holder row over, a replacement that failed after binding included). Only a pane-strike nomination waits `RECONFIRM_DELAY` (500 ms) and reconfirms pane loss; a missing pane counts only when the projection, read again after that listing, still names it as the parent's, so a swap landing meanwhile moves the watch to the replacement's pane. Other nominations check durable end state only. A live successor reseeds the member map, pane, and cursor, and an unavailable answer never cancels. Pane probes and confirmation stay on the 60 s interval, so a quiet live parent never costs a helper spawn.

The one exception is the startup relaunch. At each relaunch decision (the end of the grace before the announcement, and the end of the wait before the spawn) the wrapper awaits one fresh probe pass (`ParentWatch::probe_now`), then asks one `ParentProbe` helper unless that pass latched the end. A parent that ended between probes cancels the relaunch and settles the run `Canceled`, and an unavailable answer does not cancel. Both parent-ended cancels go through `run::cancel_and_wake_paths`, which stamps `cancel_reason = parent_ended` on the run only when its write made the run terminal; the fleet digest, `subagents list`, and the run status line print it as `its parent agent ended`. A requested probe pass reschedules the interval from that pass.

### The orphan sweep

The room host runs [`harness/orphan_sweep.rs`](../../../crates/rimz/src/harness/orphan_sweep.rs) at most once a minute (`ORPHAN_SWEEP_SCAN_TTL` in `crates/rimz/src/sidebar/refresh/mod.rs`), on a heavy-lane pass whose run-timeout step read the run list. It folds the producer's shared audit cursor once per pass and decides over the borrowed layered rollup, keeping ended parent rows without flattening history. It reads durable records only and starts hidden helpers, each of which rechecks the records before acting:

| Scan | Condition | Helper action | Diagnostic |
| --- | --- | --- | --- |
| missed digest | a live launcher whose `FleetRuns` selection is nonempty, has no live run that is not held (`FleetRuns::any_working`), and owes at least one answer | runs the same fleet reporter | `subagent_digest_backstopped` when it queues |
| abandoned peer turn | a launcher's open peer run whose row ended, whose recorded process died, or which is parked with no running or unreported runs in its own fleet and no armed wait for its session in the loop catalog | rechecks the peer. Fails an ended or dead peer's turn, or checks a parked turn twice five seconds apart through `settle_stranded_park`, then runs the fleet reporter | `subagent_digest_backstopped` when it queues |
| orphan | a live launched child whose parent agent ended (`agent_end` over the parent's launch: its holder row ended, or a replacement that failed after binding), or that has no row, `ORPHAN_GRACE` (10 minutes) ago, measured from that end stamp, the failed replacement's last activity, or the child's registration | cancels an open run with `cancel_reason = parent_ended` when the parent's row ended (not when it has none), then uses the [stop seam](./scripting.md#reclaiming-the-run-pane) to reclaim a run's pane and record its end. Without a run, closes the pane and records `rimz.subagent-orphan-reaped` | `subagent_orphan_reaped`, or `subagent_orphan_repair_failed`, which stays eligible for the next scan |

A live launcher with open team runs whose entire cohort has ended is a digest candidate too. The same helper rechecks and settles those teams ([team reports](./teams.md#team-reports)) before it settles peer turns.

Row stamps make repeated passes and races idempotent. Each diagnostic means the normal wrapper path was missed ([diagnostics.md](../diagnostics.md)).

The sweep reads no message queue, so the peer helper verifies on both checks that nothing is owed and keeps a racing wake turn intact. A provider can die before its first hook, so the peer scan also checks the recorded process's liveness, including for a `Pending` launch run. Unknown liveness is not death.

### The deadline ladder

The ladder bounds pending and running work, never a lingering terminal child. [`harness/deadline.rs`](../../../crates/rimz/src/harness/deadline.rs) evaluates only `Pending` and `Running` records. A terminal child lingers until `[agents.subagents] linger` elapses while its parent lives, and is also bounded by the watchdog, the orphan sweep, the parent stop cascade, and `rimz subagents stop`.

`deadline_at` is the soft bound, and `kill_due` fires at `deadline_at + grace`. `due_rung` selects the latest crossed warning or stop above `deadline_notice_at`, and `run::claim_rung` claims it under the workspace lock. The hook feed skips provider-native children and asks a context-capable adapter to attach the rung only on an accepted event. `stop_due_by_pane` reads what the kind declares to decide when the host sends the stop rung itself. A kind whose hook context carries the stop (`HookContextReply::carries_stop`) leaves the rung to the hook for `min(10s, grace / 2)` after the deadline; once that passes with the rung still unclaimed, the host sends it as an unforced interrupt, unless the child is waiting on input. Any other kind gets it from the host at the deadline, as that interrupt. Antigravity's injected system message carries warnings, never the stop: its hook never claims it, so the host interrupt stays its one stop channel ([adapter_antigravity.md](../agents/adapter_antigravity.md#deadline-warnings)). Every kind declares interrupt keys ([the message keys](./messaging.md#the-message-keys)), so the host stop is always an interrupt. The hook and the host claim the same rung under the lock, so the stop reaches a child by one channel.

The read-only producer in [`harness/run_timeout.rs`](../../../crates/rimz/src/harness/run_timeout.rs) spawns one hidden `agents run-timeout` helper for a pane stop, a kill, an expired interrupt hold, or a child's interrupt marker to record ([scripting.md § Interrupted subagent turns](./scripting.md#interrupted-subagent-turns)). For a pane stop, the helper stamps the rung before it sends a `HarnessNotice::Deadline` into the pane as an interrupt, and that notice never counts as an owed wake. The notice carries the run as `about_run`, so a stop the typing hold or a missing pane leaves queued is [withdrawn at delivery](./messaging.md#the-run-linked-notice-guard) once the run ends, or ends and reopens without the stop rung claimed. The producer asks no pane stop while the child's card is waiting on input or has a permission under automatic review, the test `message::dispatch` applies to an unforced interrupt, so the rung stays unclaimed for the hook to carry on the first tool call after the answer, or, for a kind whose hook context does not carry the stop, for the host interrupt once the wait clears. The helper claims the rung only while `stop_due_by_pane` still holds on the locked record. An interrupt refused because a prompt opened after the host's read (`DispatchErr::WaitingOnInput`) gives the rung back through `run::release_rung`, which restores the stamp the claim replaced unless the claim has moved on since. At the kill bound it reads the provider transcript outside the lock (nothing for a [result-file run](#response-files)), then `timeout_if_due` atomically fills an absent `last_message` and settles `TimedOut`, and the usual terminal publisher writes the partial response. The kill itself, and the pane reclaim, are the supervised run's ([scripting.md § The wake socket](./scripting.md#the-wake-socket)). Heavy-lane tick cadence bounds enforcement precision. A record without `grace` kills at `deadline_at` and never gets a stop rung.

### A child parked on a provider limit

A child whose live turn died on a provider limit keeps its run open. That covers both a provider whose turn dies with no `Stop` hook (such as a Codex rollout ending in `usage_limit_exceeded`, which leaves the row raw `running`, the run `running`, and a limit marker in the context sidecar) and an errored `TurnEnded` hook whose frame carries a marker that pauses the turn (a rate limit, spend limit, or overload, such as Antigravity's errored `Stop` or a Claude or Qwen `StopFailure`). That hold is the run fold's rule for every run that closes on a turn end, a child's or not ([completion](./scripting.md#completion-folding-lifecycle-observations)). On that errored turn end, `fold_lifecycle` keeps the run `running` with no `completed_at`, clears `interrupted_at`, `parked_at`, and `parked_on`, keeps the frame's `transcript_path` and `last_message` (a message that is only the marker's own error text, as Qwen and Qoder send it, is no answer and is not stored), ignores a Stop replayed from before `turn_opened_at`, and ends any older `limit_hold` through `end_limit_hold`. An errored turn end whose marker does not pause the turn, or that carries none, and an `Ended` signal settle the run `failed` at once and send the digest. A run that later fails on the limit takes its typed cause from the park notice below.

[`harness/park_notice.rs`](../../../crates/rimz/src/harness/park_notice.rs) is the read-only detector. On the heavy-lane tick it takes the run list `run_timeout::enforce` already read and the producer's context-joined agents. A parked child is the child of a nonterminal subagent run reporting to its launcher that is live and parked under `auto_continue::limit_marker_active` (the rate and spend classes, answering by the active rule on a running row and by the terminal rule on a raw `failed` row; an overload or budget park reads `paused` in `list` but is never classified). `park_action` decides what its run is owed:

- A run whose `limit_hold` has reached the later of its `until` and `since + FAIL_WINDOW`, whose child is still on that park (`park_notice::same_park_activity`: a `last_activity` no later than `hold.activity` and in the same second, since the producer's transcript fold cuts to the whole second and a hook applied on replay leaves the heartbeat before it; with the marker active), and whose continuation is `Off` (auto-continue off) or `Spent` (the child is in `auto_continue::exhausted_parks`) fails: `ContinueFailed` when spent, `RefillKnown` with `refill_at = until` when off. A stored hold is never reclassified.
- Otherwise `unnoticed_park` must hold: the launched parent is live, and `RunRecord::park_noticed_activity` is absent or older than the child's `last_activity`. A `limit_hold` already stored for that `last_activity` (under the same same-second rule) is claimed again, never reclassified.
- Otherwise `classify` decides from `limit_reading` for the child's `login_key` and model: `Refill(r)` when `ProviderCapacity::latest_spent_window_reset_for_model` names a refill ahead; else `Refilled` when `subscription_budget_available_for_model` holds and `agents::account::cached_usage_read_at` is newer than `since`, since an older read can predate the limit; else `Unknown`. `since` is the marker's `at` clamped into `[answer_started_at, now]`. First match wins: a refill past `now + horizon` fails with `BeyondHoldLimit`; with continuation `On`, a refill holds until it; inside `since + FAIL_WINDOW` (10 minutes: time for a paced usage read to publish the refill, and for the child or a person to continue it) nothing happens and nothing spawns; then `Spent` fails with `ContinueFailed`, a refill with `RefillKnown`, `Refilled` with continuation `Off` with `Refilled`, and anything else with `RefillUnknown`.

The horizon is `harness.cache_keepalive_max`, or 6h when that is `off`, whatever `harness.cache_keepalive` says. Each hold or fail spawns one hidden `agents park-notice` helper whose request carries the horizon and the continuation, both resolved by the host pass; the helper takes them from the request and does not read the resume messages again. The helper rereads the audit projection with rest certificates joined (`store::agent_context::attach_rest_certificates`) and child display context joined for raw-`failed` launched children (`store::agent_context::attach_child_display_context`), and folds the per-tool heartbeat onto it with `agent_activity::read_for_keys` and `SidebarSnapshot::with_agent_activity`. That is the same fold the producer's enrich applies, so both sides name the park by the same heartbeat-raised `last_activity`. It runs `park_action` again at its own `now` with its own limit reading, and that answer is the one written.

A hold is one locked write in `run::claim_limit_hold`: the claim (`park_noticed_activity`), `limit_hold { activity, since, until }`, and `deadline_at` and `deadline_notice_at` (when set) each moved later by `until - since`. A claim of a hold already stored for the same `activity` (an older binary released its claim after a failed notice) moves no stamp. A claim for another `activity` over a stored hold moves both by `until - max(since, stored until)`, so a span already added is never added twice, a later refill adds the difference, and an earlier one gives it back; a stop after the stored `until`, whose resume no hook reported, is charged from its own `since`. `due_rung`, `kill_due` and `kill_at` read the moved stamps unchanged. The fold clears the hold on the child's first in-turn signal (`TurnStarted`, `ToolUsed`, `AwaitingInput`) or next errored turn end that pauses (a limit or an overload) whose hook was received after the later of `since` and `activity`; a hook received at or before them is the stopped turn's own, applied late, and keeps the hold (`crates/rimz/src/harness/run.rs::end_limit_hold`, fed the frame's ingress time). When that comes before `until`, it first pulls both stamps back by `until - now`, so the run is charged only the time it was actually paused. A reopen clears it, and a terminal write keeps it. A hold queues no message of its own; the helper then runs the fleet reporter ([composing the digest](#composing-the-digest)).

A failure is `run::fail_limit_park`: a locked no-op on a terminal run, on a held park before its `until`, and on an unheld park a hold already claimed. Otherwise it settles `Failed` with the typed `limit_failure { cause, refill_at, label }` and its sentence from `park_notice::failure_tail` in `failure_tail`, which the digest row prints only for an answer without `limit_failure` (`limit_reason` composes the typed row), then follows the timeout helper's tail (`run_timeout::reclaim_settled_run`): wake the run, stop the provider, and stop the run's agent unless the run keeps it. `label` is the marker's provider label. A reopen moves `limit_failure` into the earlier answer beside `failure_tail`.

`last_activity` names the park because it advances on any resume. Helper races and restarts therefore hold once per activity, and a later one is classified afresh. On a raw-running row it is frozen while the turn is dead, since anything that moves it past the marker ends the park. On a raw-`failed` row a compaction, or another progress hook that opens no turn, moves it with the marker still active: that is classified again and claimed over the stored hold, with the stamps moved by the rule above. A held child draws no `SUBAGENT_STALLED`: `AgentState::silent_child_for` returns nothing while a turn error is displayed, and the stopped-child rule below leaves limit pauses to this mechanism.

### A child silent past the stall window

A silent open child is found by `crates/rimz/src/harness/stall_notice.rs::unnoticed_stall` beside the park detector, over the same heartbeat-folded producer agents and the configured stall window. Silence runs from the later of `last_activity` and the request recorded for the open turn (the clock the row's stall rung takes), and covers a running child, or a prompted launched child at raw and effective `Idle` with no `last_prompt_at` and no `turn_ended_at`: a first turn that never started (requiring `last_prompt_at` unset excludes a reopened child that a prompt had already reached). The carried `prompt` excludes promptless interactive launches, which the activity-bound `task` and launch-stamped `turn_started_at` cannot do. An unbound run finds its unended launched card by `kind` and `agent_name`, and a bound run uses only its session identity. The predicate's typed verdict selects how the notice opens, except that an unbound run whose card is still the provisional launch card is reported as never started, since its provider never reported in.

The same notice covers a child stopped on a turn error that nothing will retry, a rule `stall_notice.rs` keeps beside `silent_child_for` rather than in it, so the sidebar's silence mark and `rimz subagents list` keep showing such a child's provider error, never silence. The rule reads the displayed turn error (`AgentState::displayed_turn_error`), never its origin: a limit class is left to the park notice; a `failed`, `unknown`, or `not_entitled` class is owed the notice; a `paused_overloaded` class is owed it unless the request's `continuation` (the host pass's `park_notice::Continuation`, read from the same exhausted parks that demote a spent park to `failed` on display) is `on`. It waits for the same window of silence since `last_activity`. So a child paused on an ordinary provider overload with auto-continue off, and a child whose API-error death left its run open, also reach their parent. The notice quotes `AgentState::displayed_turn_error_label`.

The detached `crates/rimz/src/cli/agents_cmd/stall_notice.rs::run` helper rereads rest certificates and heartbeats, attaches child display context to raw-`failed` children (`store::agent_context::attach_child_display_context`, so an overload-held child's marker counts), reapplies `unnoticed_stall` with the request's window and continuation, and claims `RunRecord.stall_noticed_at` through `crates/rimz/src/harness/run.rs::claim_stall_notice`. Only a live subagent run reporting to a live launcher is eligible. The claim is once per run, not per activity stamp: recovery, another stall, and a follow-up reopen never rearm it. A failed queue write releases only that claim's timestamp. The helper queues `HarnessNotice::SubagentStalled` at `Done` for the parent's pane, links it to the claimed run by `about_run`, and makes one boundary attempt. Delivery [withdraws the notice](./messaging.md#the-run-linked-notice-guard) if that run has ended or gone first. It is a batchable notice, not a digest or an owed wake. After queueing it appends one `Assist::StallNotice`, even when delivery does not land or errors, naming the child, parent, silence, message, and delivery outcome ([assist log](./loops.md#the-assist-log)). Neither detection nor the notice stops or settles the child. The sidebar projection and its parent-turn rule live in [sub-agent lists](../sidebar/sidebar.md#sub-agent-lists).

## Follow-ups and resume

A settled child can take another turn: a live interactive child through an ordinary message, an ended one through a parent's message that resumes it. Either way the same run record reopens.

### A follow-up reopens the run

[`harness/run.rs`](../../../crates/rimz/src/harness/run.rs), `fold_lifecycle`, reopens a terminal turn-lifecycle subagent run on a `TurnStarted` of its kind and bound session, whatever its terminal status. Other terminal runs stay terminal. A `TurnStarted` inside an open [interrupt window](./scripting.md#interrupted-subagent-turns) is no reopen: the run was never terminal, and it continues as it was. The reopen:

1. moves the replaced answer into `earlier_answers` with its digest link, its `failure_tail` and its `limit_failure`, unless it was joined or reported to `nobody`, and drops joined earlier entries
2. increments `follow_ups` and sets `follow_up.started_at` to the reopen time with no prompt, then clears `opened_by`
3. sets `report_to` to `launcher` and status `Running`, clearing `completed_at`, `parked_at`, `parked_answer`, `joined_at`, `report_message_id`, `last_message`, `failure_tail`, `limit_failure`, `limit_hold`, and `deadline_notice_at` (`stall_noticed_at` stays set)
4. re-arms the deadline to `now + timeout`. A record without a stored timeout re-arms by the previous answer's own span (from `follow_up.started_at`, else `started_at`), so later reopens do not compound the timeout

The next completion is newly terminal again and can produce another digest without losing the earlier answer. Verification retries and park and unpark cycles keep the same ordinal. A message queued for the child before its clean turn end parks the run on `message` instead of letting it complete and reopen: the unpark takes step 4's deadline rearm and nothing else, so the prompt's turn joins the same answer with no new ordinal ([parked runs](./scripting.md#parked-runs)).

Delivery confirmation names the follow-up's task. Inside the [acknowledgment callback](./messaging.md#confirmation-and-retry), without reacquiring its workspace lock, `harness/run/peer.rs` stamps the subagent named by the hook's `RIMZ_RUN_ID`. Prompt-correlated and oldest-sent-batch confirmations both qualify. Only a nonterminal subagent bound to the confirming card, with neither peer nor team metadata, is written. Conversation records (human or agent senders) append their unique message ids to `opened_by`, and the first such confirmation fills an unset `follow_up.prompt` with their bodies joined by blank lines. Later deliveries, including while parked, append ids without replacing that task. Notice-only confirmations stamp nothing, and direct pane input supplies no task. A follow-up's digest row uses this prompt's first line, and its elapsed time starts at the reopen.

### Resuming an ended child

For an ended child, [`cli/message/dispatch.rs`](../../../crates/rimz/src/cli/message/dispatch.rs), `recipient_miss`, calls `resume_child` in [`launcher/child_resume.rs`](../../../crates/rimz/src/launcher/child_resume.rs) before reporting a miss, as the MCP `message` tool does. It resolves the caller through ancestry against the audit projection, and the target only among that caller's launched children, so a user shell, peer, or sibling cannot resume it this way. For promptless reopen from any shell or helper, `rimz agents resume <child>` (`cli/agents_cmd/resume.rs::resume_launched_child`) calls `reopen_child` in [`launcher/child_resume.rs`](../../../crates/rimz/src/launcher/child_resume.rs), which resolves the parent, its permission mode, and its isolation from durable records (`address::launched_parent`, `AgentState::runs_in`), runs trust preflights attended when a human calls, skips `limit_refusal` and queues no message, places the pane in the parent's subagent zone under `lock_subagent_zone`, and waits up to 10 seconds for durable bind (failing non-zero if the bind does not land) before focusing unless `--bg` or an agent called. A live launched child is focused instead of refused or opened twice, concurrent reopens yield one pane with the second returning `ReopenChildOutcome::AlreadyBound` and focusing it, and `validate_recorded_resume` keeps refusing children for `rimz agents <kind> --resume=<id>`.

`resume_resolved` checks, in order: the recorded cwd (an empty-directory checkout is re-prepared), the subagent-profile posture and the [parent caps](#the-parent-caps-a-childs-posture), the sandbox preflight for scoped skills and capability, the kind's hook installation, the login and provider session resume support, the resolved login's hook [health](./fleet.md#every-path-that-builds-a-launch-layout), folder trust for a placed checkout, launch-directory admission, a recorded run, a launch identity, and a bound parent pane, then, under the subagent zone lock, that the child is not already bound. Only after every check, and before its first write (the dispatch that queues the message, then the pane), it refuses a child whose newest run carries `limit_failure` (`child_resume::limit_refusal`) on the account `resume::resume_login` names, with no refusal when it cannot read the session. On a changed account, `park_notice::refill_ahead` refuses while it reads a refill ahead. On the child's own account (`child_resume::same_login_refill`), `park_notice::limit_reading` with `since` at the run's `completed_at` decides: `Refill` refuses until that reset, `Refilled` (quota available in a usage read newer than the failure) lets the message through, and `Unknown` falls back to the failure's own `refill_at` while it is ahead, unless the login's identity floor (`AccountStamps::floor_at_ms`) is newer than the failure. Any provider's recorded `refill_at` counts. A limit-failed child that cannot resume for another reason gets that reason first. That refusal (`ResumeChildOutcome::LimitRefusal`) is the whole error, without the `cannot resume` prefix, and `recipient_miss` records it as the unresolved message's reason. The caps refuse a replayed mode above the parent's current one and never lower the saved grant. A missing session, unsupported resume, or degraded posture is a miss with its reason, never a fresh launch.

The request reuses the child's session, launch identity, newest run, and keep policy, and takes cwd from the child without taking checkout ownership. Placement uses the parent's session and the ordinary subagent zone fallbacks. Under the zone lock, an already-live card returns to normal dispatch. Otherwise the follow-up is queued for the named ended child before the split, and the queue commit decides:

- start a resume, with `register_by` two minutes after its enqueue (`RIMZ_MESSAGE_REGISTER_TIMEOUT_MS` overrides the limit), or
- join an unexpired queued registration wait on the same card, copying its earliest deadline

Only a start opens a pane. Joiners never extend the wait, and a follow-up at or after its deadline starts a new resume without needing a sweep. Steer and interrupt become the child's next boundary prompt. A failed split errors that same record and returns the error, without an unresolved-message bounce. A confirmed opening keeps the record regardless of the three-second bind check, which is placement pacing and diagnostic evidence only. There is no second dispatch.

The receipt names the message id, the resuming child, and the absolute UTC registration deadline. The message is the next prompt, `TurnStarted` reopens the run, and the idle-close cleanup rule applies again. An explicit stop, a failure, or a timeout does not prevent a later parent-message resume when its preconditions pass.

Every subagent resume reuses a run id, so the exec wrapper supervises it. The wrapped resume appends `rimz.agent-resumed` with `Registered` after recording its pane and before spawning the provider, as every resume does ([model.md](../agents/model.md)). That makes the row live without waiting for a lazily registering provider's first hook. Attach alone changes placement, not lifecycle. After spawning the provider and recording its pane again, every resumed wrapper triggers queued-head delivery through the same library helper as the registration hook reactor ([messaging.md](./messaging.md#what-triggers-a-delivery)), since the plain resume stamp does not invoke that reactor.

### The receiving-turn hold

When the reused run is terminal before spawn and a follow-up message started the resume (`ExecRequest.follow_up_message` is set), the wrapper holds idle cleanup until `TurnStarted` reopens it. The durable follow-up ordinal detects the reopen even when the turn completes between monitor polls. While the ordinal has not advanced, a `Queued`, `Claimed`, `Sent`, or `Delivered` message keeps the hold, and any other terminal status or a missing record releases it to idle-close settlement with `skip_linger: true` on `Duty::Receipt`, skipping the `[agents.subagents] linger` wait while keeping the report, open-turn, queued-message, and focus checks. That exemption ends as soon as a turn opens on that wrapper (`follow_ups` advances past the awaited ordinal or the run leaves terminal status), so any later finish lingers normally; a hold that drops because the follow-up opened a turn takes no exemption. The wrapper reads that status without a store fold, under the workspace lock across queue and retained history, and caches it until the queue stamp changes. An unreadable record keeps the hold until that stamp changes too. A wrapper started on a terminal run without a follow-up message takes no reopen hold and closes under the ordinary `linger` rule measured from its bind. A failed follow-up does not rewrite or report the already-reported answer.

### Messages for a child that ended

The sweep archives a queued message whose child has ended, including ends stamped directly by the wrapper, the reaper, or rebirth rather than a provider hook. A provider end hook archives first, through the lifecycle reactor. Both name `rimz message @<handle>` as the parent's resume-and-resend path, through one reason formatter. If the parent resends before archival, the resumed child's older queued head stays first in FIFO order. A missing pane without an end stamp keeps the ordinary no-pane retry.

## Signals are not the settlement path

A child's lifecycle transitions also fire `agent.*` signals ([loops.md](./loops.md#the-signal-vocabulary)) carrying `session`, `handle`, and, when recorded, the child's `parent_agent_id` in `parent`: the parent's launch id, or its session id for a parent without one. A parent learns of its children through the digest and `wait`, which carry the child's text and run identity; a signal carries only the transition, and no command arms one for a parent to wait on.

## What is left out

There are no `subagents restart` or `subagents resume` verbs. `rimz agents resume <child>` reopens an ended launched child into its live parent's subagent zone without queuing a message, parent-message resume continues an existing session with a follow-up, and when preconditions fail, launching the same profile and prompt creates a new child.

`agents restart @child` and `agents explain @child` read the row's launched-child lineage to retain child launch treatment: the child reminder replaces the catalog and team context, the delegation skill is withheld, and native delegation is disabled. The profile uses the [doorway rule](./fleet.md#posture). The restarted pane closes when the provider exits instead of dropping to a shell. `restart_resolved` picks the child's run as the resume doorway does (`newest_run_for_child`) and sorts it in `open_run_fate`: a nonterminal run whose session resumes, unless an interrupted turn skips its continue (`--no-continue`, or a blank `resume.auto_continue_text`, read through `exec.rs::continue_text`), is stamped `handed_over_from` with the old pane (`run::hand_over`) after the last refusal and before the split, cleared again if the focus or split fails, and the replacement's exec request carries it with its exit policy (`with_run`), so the parent's report carries the replacement's answer. The replacement records its pane on the run only once its bind stands, and its launch error fails the run only from that pane (`run::fail_if_owned`), so a replacement whose bind is refused leaves the run with the old session. A workflow leader's open session run is handed over the same way by every restart and molt ([workflow.md § A session's end](./workflow.md#a-sessions-end)), and is never failed by a fresh restart. After the old pane closes, the parent gets one `SUBAGENT_RESTARTED` (`notify_parent_restarted`) unless it ran the restart. From then on `RunRecord::superseded_in` names every stale generation: the stamped pane, and, while a stamp is set, any pane other than the one bound to the run since. The lifecycle fold ignores every observation from such a pane (a late `Stop`, `SessionEnd`, or in-turn signal of the replaced provider), and the old wrapper's exit neither settles nor reports the run: `exec.rs::settle_exited_run` reads the rule before and after its hook drain and grace wait, and the failure write rechecks it under the record lock (`run::fail_if_owned`). The stamp stays after the replacement binds, since only it keeps the bound-pane test on, and a later restart overwrites it; a failed split clears it, even on a run settled meanwhile, and so does a new wrapper binding the stamped pane id again (`run::record_pane`). Otherwise (a fresh restart, or a skipped continue on an interrupted turn) restart fails the run with a reason after the split and before the close, and the old wrapper's exit reports it. Standalone resume and rebirth do not plan launched children ([fleet.md § Resume entry points](./fleet.md#resume-entry-points)), and [fleet.md § Rebirth](./fleet.md#rebirth) owns crash-time run settlement. Parent-message resume shares that profile selection without changing generic recovery.

One-shot providers cannot be kept interactive by the wrapper. A follow-up after their exit requires provider session resume support.

## See also

- [scripting.md](./scripting.md): the supervised run every child is, including parks and pane reclamation.
- [fleet.md](./fleet.md): the launch, address, and reclaim machinery, the startup relaunch, and the launch reminder order and channels.
- [model.md](../agents/model.md): the rollup, and the provider-native subagent rows this page's predicates exclude.
- [cli/subagents.md](../../reference/cli/subagents.md): the user-facing command and flag surface.

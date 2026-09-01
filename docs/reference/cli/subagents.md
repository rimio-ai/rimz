# Subagents

`rimz subagents` provides launch and lifecycle verbs for delegating one bounded prompt to a supervised child, plus read-only discovery from any shell. It is syntax sugar over `rimz agents`: the child gets a real pane, durable run record, petname, parent link, and sidebar entry without making the parent choose the supervision flags.

Launch, `fanout`, `wait`, and `stop` run only from a RimZ-launched agent. A user-shell invocation of one of those verbs fails before opening the room and points to `rimz agents` or `rimz teams`; the read-only `list` and `profiles` verbs work in either context. The mechanics behind the sugar — the direct-parent stamp, no-further-launch rule, caller-scoped lifecycle verbs, and what closes a finished child — are in [subagents.md](../../internals/harness/subagents.md).

A child launched through this doorway must complete its assignment directly and cannot spawn further agents. RimZ appends that instruction to the provider's system prompt when it has a native launch flag, falls back to the user prompt for other providers, disables the provider's native delegation tool where a verified restriction exists, and refuses both `rimz agents` and `rimz subagents` when the caller is itself a subagent.

## Launch and fan out work

```sh
rimz subagents fanout tasks.json
printf '%s\n' '[{"profile":"codex","prompt":"find the smallest safe fix"}]' \
  | rimz subagents fanout
rimz subagents fanout tasks.json --wait

rimz subagents fanout --wait <<'JSON'
[
  {"profile":"codex","prompt":"review correctness; report concrete findings"},
  {"profile":"claude","prompt_file":"/tmp/review-brief.md"}
]
JSON
```

`fanout` reads a JSON task array from `FILE`, or from stdin when `FILE` is omitted. It validates the whole list and opens each child pane in sequence. The children run in parallel after their panes open, and each minted petname prints as it launches.

By default, `fanout` returns after launching. Once every launched child in the parent's current fleet has settled, one status-only `SUBAGENT_REPORT` digest from `@rimz` lists their outcomes and names the exact `rimz subagents wait @…` command for those rows. Use `--wait[=DURATION]` to join exactly the children from that fanout, optionally with a caller-side deadline; each answer prints as it finishes under a `--- petname ---` header using the shared [agent-prose rendering rule](../cli.md#agent-prose), with a status suffix only for an abnormal outcome, and the command exits nonzero if any child does. Waiting reads durable results; if every child listed by one queued digest prints inline, RimZ cancels that redundant digest, while any unread row keeps it queued. This wait deadline is distinct from the children's `--timeout`. With background fanout `--json`, RimZ emits a map from petname to `run_id`; with `--wait --json`, fanout emits the same labeled result map as a plural `rimz subagents wait --json`, including each run's `last_message` when available.

Each array entry has the single-launch fields that make sense for data-driven delegation:

| Field | Required | Meaning |
| --- | --- | --- |
| `profile` | yes | A configured subagent profile, agent kind, or shared command |
| `prompt` | exactly one | Complete task supplied by the parent |
| `prompt_file` | exactly one | File whose contents become the prompt; exclusive with `prompt` |
| `name` | no | Durable child petname |
| `model` | no | Model override |
| `agent` | no | Profile or provider-kind rebase |
| `effort` | no | Reasoning effort |
| `timeout` | no | This child's deadline |
| `max_turns` | no | Maximum agentic turns |
| `description` | no | Initial child-card description |

Explicit names must be unique within the list. Relative `prompt_file` paths resolve from the caller's current working directory. A task timeout overrides the fanout-level `--timeout`, which overrides `[agents.subagents] timeout` (30 minutes by default). `--keep` applies to every child. Per-task raw argv, execution mode, and pane retention are deliberately omitted; use `[subagents.profiles]` for provider arguments or separate single launches when children need different lifecycle controls.

All tasks are validated before the first launch. If a runtime failure occurs after some children have started, the error names them; they keep their normal deadline and cleanup behavior and remain available to `subagents wait` and `subagents stop`.

## Launch one child

```sh
rimz subagents claude "trace the authentication call path"
```

Each launch is equivalent to a one-cell `rimz agents <profile> <prompt> -p --bg` run with a timeout. It prints the minted petname immediately and writes a callback notice to stderr, so a parent can start several children without waiting between launches. Pass `--wait[=DURATION]` to print the petname and then join the child like `subagents wait <name>`, including its final message or failure tail. `--json` is accepted on a single launch only with `--wait`; it emits the full run record, the same shape as `subagents wait <name> --json`, without the human petname line.

```sh
first=$(rimz subagents codex "find the smallest safe fix")
second=$(rimz subagents launch reviewer "review the proposed API")
rimz subagents wait "$first" "$second"
```

The bare form and `launch` verb are equivalent. A prompt is mandatory: the parent must supply the whole assignment as the second positional argument or with `--prompt-file PATH`. Relative paths resolve from the caller's current working directory. The child inherits the parent's current checkout and lane.

| Behavior | Default | Override |
| --- | --- | --- |
| Result | one status digest from `@rimz` after the fleet settles; read text with `rimz subagents wait` | `--wait[=DURATION]` joins inline when it reaches the result |
| Checkout | caller's current checkout | fixed |
| Deadline | 30 minutes | `--timeout`, then `[agents.subagents] timeout` |
| Pane after completion | closes when the run settles | `--keep` holds it until `stop` or `rimz gc` |
| Address | minted petname | `--name/-n` |

A finished child's in-pane wrapper stamps its durable end and closes the pane after stopping the provider. The run result remains joinable, and the finished child stays on the parent card until the parent's next prompt boundary. `--keep` instead holds the pane after completion and past parent exit, leaving reclamation to `stop` or `rimz gc`.

The single-launch surface deliberately omits `--worktree`, `--from-pr`, `--channel`, `--stdin`, `--resume`, placement flags, output/input formats, retries, and verification. Use `rimz agents` when the launch needs those controls; use `rimz teams` when the workers are peers rather than children.

Model, provider/profile rebasing, effort, description, turn cap, and raw provider arguments remain available. Run `rimz subagents launch --help` for their exact spellings.

## Discover agent profiles

```sh
rimz subagents profiles
rimz subagents profiles --path
rimz subagents profiles --json --path
```

`profiles` lists `[subagents.profiles]` profiles and configured launch commands available for one child as compact cards. Profile cards include their optional descriptions; `--path` adds the defining-file path. JSON also omits `path` unless `--path` is passed, keeps that path absolute, and includes `source` (`profile` or `command`). `[agents.profiles]` entries are excluded. Registered agent kinds remain directly launchable but are omitted from this catalog. It also works from a user shell.

Inside an agent whose `[agents.profiles]` entry sets `subagents = [...]`, the catalog includes only listed profiles. A launch naming any other positional profile or `--agent` rebase is refused before RimZ creates a run or pane. Team names are excluded because a subagent launch creates one agent, not a cohort.

RimZ places this same filtered catalog in each launched agent's system reminder when its adapter supports native appended system text. With no configured profiles or commands, the reminder says that no subagent profiles are configured instead of showing an empty list.

## Reports

Background children signal settlement through the ordinary durable message queue. Once no launched child in the parent's fleet has a non-terminal newest run, RimZ parks one status-only digest until the parent can receive at a successful or idle boundary:

```text
Type: SUBAGENT_REPORT
From: @rimz
Content:
All 3 settled, read with `rimz subagents wait @naming @runtime @slow-reviewer`.

@naming — completed in 4m12s, 84 lines — map spec/profile surfaces
@runtime — completed in 5m03s, 1 line — inspect runtime behavior
@slow-reviewer — timed out after 30m, 12 lines; provider did not stop — review correctness
```

For more than one child the header is ``All {n} settled, read with `rimz subagents wait @a @b …`.``; for one it is ``Your subagent settled, read with `rimz subagents wait @a`.`` The command names exactly the rows in that digest and is never a bare wait, which would join every recorded child. Rows use ``@{name} — {status_label} {in|after} {elapsed}, {size} — {description}``: timed-out runs use `after`, every other status uses `in`, elapsed time is compact, and the optional description is the launcher's description rather than the profile. Rows follow child registration order and duplicate audit rows for one run are deduplicated by run id.

For every status, a non-blank `last_message` contributes its number of non-empty lines (`1 line` or `{N} lines`); a blank value reads `no result`. A non-completed status then appends `; {reason}` when `failure_tail` has a non-empty line, using its last non-empty line. The digest never carries a child's result text or JSON; use `rimz subagents wait --json` for structured results.

A child launched while the fleet is still running joins that fleet; one launched after digest composition starts belongs to the next. Children already printed by a join are excluded from a digest that has not yet been composed. RimZ stamps every listed row with the digest id before queueing, so a join cannot cancel a half-observed row set. For a queued digest, a wait cancels only that digest and only after every listed child has printed inline; a digest with any unread row stands. Simultaneous last settlers race on first-writer-wins stamps, so each row appears at most once and none is lost. No digest is sent when the parent has ended. Durable run records remain the result truth: `list` shows them and `wait` can read their full results after panes close, and the elected producer's once-per-minute orphan scan reconstructs a missed digest from those records.

## Join results manually

```sh
rimz subagents wait
rimz subagents wait calm-fox bright-owl
rimz subagents wait --any
rimz subagents wait calm-fox --stream
rimz subagents wait --json
```

The fleet digest carries statuses only, so `wait` is the read path for a result needed before continuing or for durable history. With no names, it joins every supervised child recorded beneath the caller, including children that finished before the command started; `--any` instead considers only children still running, since it reports the first to finish. Explicit names must resolve inside that same set. A single result prints as a bare answer; plural and `--any` waits label each answer with its child name. Joins, streaming, JSON, timeout behavior, output, and exit codes are the same durable machinery as [`rimz agents wait`](./agents.md#wait).

The result is available as soon as the run settles and remains available after the pane closes, because the run record, not the pane, is truth.

## Inspect and drive children

```sh
rimz subagents
rimz subagents list --json
rimz subagents stop calm-fox
rimz subagents stop --all
```

Bare `rimz subagents` is the same read-only operation as `list`. Inside an agent it lists that agent's own RimZ-launched children, including completed children retained in durable history. From a user shell it lists every RimZ-launched child in the current channel; when the shell has no current channel, it lists children across all channels. Provider-native subagents are not part of either list.

In a user shell, each human-readable row names the child's parent and channel alongside its live status, newest supervised-run outcome, and current one-line description. The agent-scoped table keeps its compact four columns; JSON includes `parent` and `channel` in both scopes. A plain user shell in the project directory cannot derive an in-place team's stamped `<directory>/<team>` lane because that lane is carried by the launched panes rather than the shared directory. Such a shell has no current channel, so `list` deliberately broadens to all channels and the reported channel distinguishes the rows.

`stop` remains agent-only and accepts only live children of that caller.

`restart` and `resume` are deliberately absent in v1: the durable run record does not yet retain every launch argument needed to reproduce the supervised deadline, wait, and self-close contracts. Relaunch the same profile and prompt to start a fresh child, matching the Agent-tool model.

Stopping a parent through `rimz agents stop` stops its live pane-backed children first. The same cascade applies when `rimz teams stop` stops that parent.

Every child is addressable as `@<petname>`. A supervised print-mode provider is not an interactive message consumer, so do not depend on mid-run steering. A message can park against the address, but v1 does not automatically resume a finished child to consume it.

## Parentage and sidebar placement

Only `rimz subagents` creates a parented pane-backed child. The child appears in the subagent section nested under its direct parent and is not duplicated as a top-level card. Its entry shows the petname, launch profile, description, and own session cost; that spend rolls into the parent's live and lifetime figures in the sidebar, `agents show`, and teams. `agents attribution` lists it only in the subagent breakdown, outside member and total figures. Provider-native children share the product term *subagent* and the same nested presentation, but their cost is already inside the parent transcript.

Subagent launches are not capped by `[agents] max-chain-length`; that setting governs successive top-level peer launches through `rimz agents` and `rimz teams`. Instead, a subagent cannot launch anything through either doorway. A refused call creates no run, pane, worktree, or provisional agent.

Pane-backed children also share a physical zone instead of repeatedly reshaping the caller's view. A solo parent's first child opens in a right-hand column and later children stack there (native Zellij stacks, equal-height tmux panes). A member of a launched team sends its children to one companion `<view> subagents` tab shared by that team's view on both backends; the companion opens immediately after the launcher's tab. If a solo child column has no room for another split, RimZ falls back to the companion tab, then to a generic run tab if needed. If a team companion is full, the overflow child opens in a generic run tab rather than failing the run or creating a second companion. Once the last child pane closes, the companion sidebar exits and the empty tab collapses with it.

## Configure launch defaults

```toml
[agents.subagents]
timeout = "30m"
```

`timeout` uses the CLI duration syntax (`s`, `m`, `h`, `d`). The deadline is stored on each run and enforced by the room producer even if the parent never calls `wait`. A stale `budget` key in this table is rejected at config load; spend limits belong to the parent.

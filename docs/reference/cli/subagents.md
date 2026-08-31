# Subagents

`rimz subagents` provides agent-only launch and lifecycle verbs for delegating one bounded prompt to a supervised child. It is syntax sugar over `rimz agents`: the child gets a real pane, durable run record, petname, parent link, and sidebar entry without making the parent choose the supervision flags.

Launch and lifecycle commands run only from a RimZ-launched agent. A user-shell invocation fails before opening the room and points to `rimz agents` or `rimz teams`; the read-only `profiles` catalog is available from either context. The mechanics behind the sugar — the direct-parent stamp, no-further-launch rule, caller-scoped verbs, and what closes a finished child — are in [subagents.md](../../internals/harness/subagents.md).

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

By default, `fanout` returns after launching and each child reports back as a `SUBAGENT_REPORT` message when it settles. Use `--wait[=DURATION]` to join exactly the children from that fanout instead, optionally with a caller-side deadline; each answer prints as it finishes under a `--- petname ---` header using the shared [agent-prose rendering rule](../cli.md#agent-prose), with a status suffix only for an abnormal outcome, and the command exits nonzero if any child does. A joined child sends no completion report. This wait deadline is distinct from the children's `--timeout`. With background fanout `--json`, RimZ emits a map from petname to `run_id`; with `--wait --json`, fanout emits the same labeled result map as a plural `rimz subagents wait --json`, including each run's `last_message` when available.

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
| Result | reported back as a `SUBAGENT_REPORT` message when the child settles | `--wait[=DURATION]` joins inline and sends no report |
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

A background child returns through the ordinary durable message queue. RimZ parks the report until the parent can receive at a successful or idle boundary, using the child as its sender:

```text
Type: SUBAGENT_REPORT
From: @naming
Content:
@naming (explorer · map profile surfaces) completed in 4m12s.
1 subagent still running: @runtime.

Done.
```

The first line names the child, its profile and description when available, outcome, and elapsed time. The next line says whether siblings are still running, followed by the final message for a completed run or failure detail and transcript path for an abnormal outcome. Sibling state is read when each report is composed, so children settling at the same instant may each truthfully say that all subagents have finished.

No report is sent when the launch uses `--wait` or the parent has ended. The run record remains the fallback truth: `list` shows it and `wait` can still read it after the pane closes.

## Join results manually

```sh
rimz subagents wait
rimz subagents wait calm-fox bright-owl
rimz subagents wait --any
rimz subagents wait calm-fox --stream
rimz subagents wait --json
```

Background launches normally call the parent back, so `wait` is the manual re-join for a result needed before continuing or for reading durable history again. With no names, it joins every supervised child recorded beneath the caller, including children that finished before the command started; `--any` instead considers only children still running, since it reports the first to finish. Explicit names must resolve inside that same set. A single result prints as a bare answer; plural and `--any` waits label each answer with its child name. Joins, streaming, JSON, timeout behavior, output, and exit codes are the same durable machinery as [`rimz agents wait`](./agents.md#wait).

The result is available as soon as the run settles and remains available after the pane closes, because the run record, not the pane, is truth.

## Inspect and drive children

```sh
rimz subagents
rimz subagents list --json
rimz subagents stop calm-fox
rimz subagents stop --all
```

Bare `rimz subagents` lists the caller's children, including completed children retained in durable history, with live status, the newest supervised-run outcome, and each child's current one-line description. `stop` accepts only live children of the caller.

`restart` and `resume` are deliberately absent in v1: the durable run record does not yet retain every launch argument needed to reproduce the supervised deadline, wait, and self-close contracts. Relaunch the same profile and prompt to start a fresh child, matching the Agent-tool model.

Stopping a parent through `rimz agents stop` stops its live pane-backed children first. The same cascade applies when `rimz teams stop` stops that parent.

Every child is addressable as `@<petname>`. A supervised print-mode provider is not an interactive message consumer, so do not depend on mid-run steering. A message can park against the address, but v1 does not automatically resume a finished child to consume it.

## Parentage and sidebar placement

Only `rimz subagents` creates a parented pane-backed child. The child appears in the subagent section nested under its direct parent and is not duplicated as a top-level card. Its entry shows the petname, launch profile, description, and own session cost; that spend rolls into the parent's live and lifetime figures. Provider-native children share the product term *subagent* and the same nested presentation, but their cost is already inside the parent transcript.

Subagent launches are not capped by `[agents] max-chain-length`; that setting governs successive top-level peer launches through `rimz agents` and `rimz teams`. Instead, a subagent cannot launch anything through either doorway. A refused call creates no run, pane, worktree, or provisional agent.

Pane-backed children also share a physical zone instead of repeatedly reshaping the caller's view. A solo parent's first child opens in a right-hand column and later children stack there (native Zellij stacks, equal-height tmux panes). A member of a launched team sends its children to one companion `<view> subagents` tab shared by that team's view on both backends; the companion opens immediately after the launcher's tab. If a solo child column has no room for another split, RimZ falls back to the companion tab, then to a generic run tab if needed. If a team companion is full, the overflow child opens in a generic run tab rather than failing the run or creating a second companion. Once the last child pane closes, the companion sidebar exits and the empty tab collapses with it.

## Configure launch defaults

```toml
[agents.subagents]
timeout = "30m"
```

`timeout` uses the CLI duration syntax (`s`, `m`, `h`, `d`). The deadline is stored on each run and enforced by the room producer even if the parent never calls `wait`. A stale `budget` key in this table is rejected at config load; spend limits belong to the parent.

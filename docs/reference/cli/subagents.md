# Subagents

`rimz subagents` provides agent-only launch and lifecycle verbs for delegating one bounded prompt to a supervised child. It is syntax sugar over `rimz agents`: the child gets a real pane, durable run record, petname, parent link, and sidebar entry without making the parent choose the supervision flags.

Launch and lifecycle commands run only from a RimZ-launched agent. A user-shell invocation fails before opening the room and points to `rimz agents` or `rimz teams`; the read-only `specs` catalog is available from either context. The mechanics behind the sugar — the direct-parent stamp, no-further-launch rule, caller-scoped verbs, and what closes a finished child — are in [subagents.md](../../internals/harness/subagents.md).

A child launched through this doorway must complete its assignment directly and cannot spawn further agents. RimZ appends that instruction to the provider's system prompt when it has a native launch flag, falls back to the user prompt for other providers, disables the provider's native delegation tool where a verified restriction exists, and refuses both `rimz agents` and `rimz subagents` when the caller is itself a subagent.

## Launch and fan out work

```sh
rimz subagents fanout tasks.json
printf '%s\n' '[{"spec":"codex","prompt":"find the smallest safe fix"}]' \
  | rimz subagents fanout
rimz subagents fanout tasks.json --wait

rimz subagents fanout --wait <<'JSON'
[
  {"spec":"codex","prompt":"review correctness; report concrete findings"},
  {"spec":"claude","prompt_file":"/tmp/review-brief.md"}
]
JSON
```

`fanout` reads a JSON task array from `FILE`, or from stdin when `FILE` is omitted. It validates the whole list and opens each child pane in sequence. The children run in parallel after their panes open, and each minted petname prints as it launches.

By default, `fanout` returns after launching. Use `--wait[=DURATION]` to join exactly the children from that fanout, optionally with a caller-side deadline; each answer prints as it finishes under a `--- petname ---` header, with a status suffix only for an abnormal outcome, and the command exits nonzero if any child does. This wait deadline is distinct from the children's `--timeout`. With background fanout `--json`, RimZ emits a map from petname to `run_id`; with `--wait --json`, fanout emits the same labeled result map as a plural `rimz subagents wait --json`, including each run's `last_message` when available.

Each array entry has the single-launch fields that make sense for data-driven delegation:

| Field | Required | Meaning |
| --- | --- | --- |
| `spec` | yes | Agent kind or configured profile |
| `prompt` | exactly one | Complete task supplied by the parent |
| `prompt_file` | exactly one | File whose contents become the prompt; exclusive with `prompt` |
| `name` | no | Durable child petname |
| `model` | no | Model override |
| `agent` | no | Profile or provider-kind rebase |
| `effort` | no | Reasoning effort |
| `timeout` | no | This child's deadline |
| `max_turns` | no | Maximum agentic turns |
| `description` | no | Initial child-card description |

Explicit names must be unique within the list. Relative `prompt_file` paths resolve from the caller's current working directory. A task timeout overrides the fanout-level `--timeout`, which overrides `[agents.subagents] timeout` (30 minutes by default). `--keep` applies to every child. Per-task raw argv, execution mode, and pane retention are deliberately omitted; use profiles for provider arguments or separate single launches when children need different lifecycle controls.

All tasks are validated before the first launch. If a runtime failure occurs after some children have started, the error names them; they keep their normal deadline and cleanup behavior and remain available to `subagents wait` and `subagents stop`.

## Launch one child

```sh
rimz subagents claude "trace the authentication call path"
```

Each launch is equivalent to a one-cell `rimz agents <spec> <prompt> -p --bg` run with a timeout. It prints the minted petname immediately, so a parent can start several children without waiting between launches. Pass `--wait[=DURATION]` to print the petname and then join the child like `subagents wait <name>`, including its final message or failure tail. `--json` is accepted on a single launch only with `--wait`; it emits the full run record, the same shape as `subagents wait <name> --json`, without the human petname line.

```sh
first=$(rimz subagents codex "find the smallest safe fix")
second=$(rimz subagents launch reviewer "review the proposed API")
rimz subagents wait "$first" "$second"
```

The bare form and `launch` verb are equivalent. A prompt is mandatory: the parent must supply the whole assignment as the second positional argument or with `--prompt-file PATH`. Relative paths resolve from the caller's current working directory. The child inherits the parent's current checkout and lane.

| Behavior | Default | Override |
| --- | --- | --- |
| Execution | supervised print mode, background | `--wait[=DURATION]` joins and prints the result |
| Checkout | caller's current checkout | fixed |
| Deadline | 30 minutes | `--timeout`, then `[agents.subagents] timeout` |
| Pane after completion | close | `--keep` |
| Address | minted petname | `--name/-n` |

A finished child's in-pane wrapper closes its pane from the durable terminal record, independently of the parent waiting for the result. `--keep` opts out and leaves the pane for `stop` or `rimz gc`.

The single-launch surface deliberately omits `--worktree`, `--from-pr`, `--channel`, `--stdin`, `--resume`, placement flags, output/input formats, retries, and verification. Use `rimz agents` when the launch needs those controls; use `rimz teams` when the workers are peers rather than children.

Model, provider/profile rebasing, effort, description, turn cap, and raw provider arguments remain available. Run `rimz subagents launch --help` for their exact spellings.

## Discover agent specs

```sh
rimz subagents specs
rimz subagents specs --json
```

`specs` lists every agent kind, configured profile, and configured launch command available for one child. It also works from a user shell; `types` remains an alias for compatibility. Team names are excluded because a subagent launch creates one agent, not a cohort.

## Join results

```sh
rimz subagents wait
rimz subagents wait calm-fox bright-owl
rimz subagents wait --any
rimz subagents wait calm-fox --stream
rimz subagents wait --json
```

With no names, `wait` joins every supervised child recorded beneath the caller, including children that finished before the command started; `--any` instead considers only children still running, since it reports the first to finish. Explicit names must resolve inside that same set. A single result prints as a bare answer; plural and `--any` waits label each answer with its child name. Joins, streaming, JSON, timeout behavior, output, and exit codes are the same durable machinery as [`rimz agents wait`](./agents.md#wait).

The result remains available after the child pane closes because the run record, not the pane, is truth.

## Inspect and drive children

```sh
rimz subagents
rimz subagents list --json
rimz subagents stop calm-fox
rimz subagents stop --all
```

Bare `rimz subagents` lists the caller's children, including retained completed children, with live status, the newest supervised-run outcome, and each child's current one-line description. `stop` accepts only live children of the caller.

`restart` and `resume` are deliberately absent in v1: the durable run record does not yet retain every launch argument needed to reproduce the supervised deadline, wait, and self-close contracts. Relaunch the same spec and prompt to start a fresh child, matching the Agent-tool model.

Stopping a parent through `rimz agents stop` stops its live pane-backed children first. The same cascade applies when `rimz teams stop` stops that parent.

Every child is addressable as `@<petname>`. A supervised print-mode provider is not an interactive message consumer, so do not depend on mid-run steering. A message can park against the address, but v1 does not automatically resume a finished child to consume it.

## Parentage and sidebar placement

Only `rimz subagents` creates a parented pane-backed child. The child appears in the subagent section nested under its direct parent and is not duplicated as a top-level card. Provider-native children share the product term *subagent* and the same nested presentation.

Subagent launches are not capped by `[agents] max-chain-length`; that setting governs successive top-level peer launches through `rimz agents` and `rimz teams`. Instead, a subagent cannot launch anything through either doorway. A refused call creates no run, pane, worktree, or provisional agent.

Pane-backed children also share a physical zone instead of repeatedly reshaping the caller's view. A solo parent's first child opens in a right-hand column and later children stack there (native Zellij stacks, vertical tmux panes). A member of a launched team sends its children to one companion `<view> subagents` tab shared by that team's view on both backends. If a solo child column has no room for another split, RimZ falls back to the companion tab, then to a generic run tab if needed. If a team companion is full, the overflow child opens in a generic run tab rather than failing the run or creating a second companion.

## Configure launch defaults

```toml
[agents.subagents]
timeout = "30m"
```

`timeout` uses the CLI duration syntax (`s`, `m`, `h`, `d`). The deadline is stored on each run and enforced by the room producer even if the parent never calls `wait`. A stale `budget` key in this table is rejected at config load; spend limits belong to the parent.

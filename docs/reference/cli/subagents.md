# Subagents

`rimz subagents` is the agent-only doorway for delegating one bounded prompt to a supervised child. It is syntax sugar over `rimz agents`: the child gets a real pane, durable run record, petname, parent link, and sidebar entry without making the parent choose the supervision flags.

Run it only from a RimZ-launched agent. A user-shell invocation fails before opening the room and points to `rimz agents` or `rimz teams`. The mechanics behind the sugar — the ancestry stamp and its depth cap, the caller-scoped verbs, and what closes a finished child — are in [subagents.md](../../internals/harness/subagents.md).

## Launch and fan out work

```sh
rimz subagents claude "trace the authentication call path"
```

Each launch is equivalent to a one-cell `rimz agents <spec> <prompt> -p` run with a timeout. It blocks until the child finishes and prints its result. Pass `--bg` to print the minted petname immediately, so a parent can start several children without waiting between launches.

```sh
first=$(rimz subagents codex "find the smallest safe fix" --bg)
second=$(rimz subagents launch reviewer "review the proposed API" --bg)
rimz subagents wait "$first" "$second"
```

The bare form and `launch` verb are equivalent. A prompt is mandatory: the parent must supply the whole assignment as the second positional argument. The child inherits the parent's current checkout and lane.

| Behavior | Default | Override |
| --- | --- | --- |
| Execution | supervised print mode, blocking | `--bg` returns immediately with the child's petname |
| Checkout | caller's current checkout | fixed |
| Deadline | 30 minutes | `--timeout`, then `[agents.subagents] timeout` |
| Pane after completion | close | `--keep` |
| Address | minted petname | `--name/-n` |

A finished child's in-pane wrapper closes its pane from the durable terminal record, independently of the parent waiting for the result. `--keep` opts out and leaves the pane for `stop` or `rimz gc`.

The launch surface deliberately omits `--worktree`, `--from-pr`, `--channel`, `--stdin`, `--top-level`, `--resume`, placement flags, output/input formats, retries, and verification. Use `rimz agents` when the launch needs those controls; use `rimz teams` when the workers are peers rather than children.

Model, provider/profile rebasing, effort, description, turn cap, and raw provider arguments remain available. Run `rimz subagents launch --help` for their exact spellings.

## Discover agent types

```sh
rimz subagents types
rimz subagents types --json
```

`types` lists every agent kind, configured profile, and configured launch command available for one child. Team names are excluded because a subagent launch creates one agent, not a cohort.

## Join results

```sh
rimz subagents wait
rimz subagents wait calm-fox bright-owl
rimz subagents wait --any
rimz subagents wait calm-fox --stream
rimz subagents wait --json
```

With no names, `wait` joins every supervised child recorded beneath the caller, including children that finished before the command started; `--any` instead considers only children still running, since it reports the first to finish. Explicit names must resolve inside that same set. Joins, streaming, JSON, timeout behavior, output, and exit codes are the same durable machinery as [`rimz agents wait`](./agents.md#wait).

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

## Depth and sidebar placement

A `rimz subagents` launch counts as one normal agent-launch depth. `[agents] max-launch-depth` defaults to one, so a top-level agent may launch children and those children may not launch grandchildren. An over-limit call refuses before creating a run, pane, or provisional child.

When `max-launch-depth` is raised above one, RimZ retains true depth but flattens display ancestry under the original top-level agent. The caller-scoped verbs follow that durable display ancestry: the top-level agent sees all descendants, while an intermediate child does not get a separate direct-child set.

Provider-native children and RimZ-launched pane-backed children share the product term *subagent*: both appear in the subagent section nested under the top-level parent's card. No pane-backed child is duplicated as a top-level card.

## Configure launch defaults

```toml
[agents.subagents]
timeout = "30m"
```

`timeout` uses the CLI duration syntax (`s`, `m`, `h`, `d`). The deadline is stored on each run and enforced by the room producer even if the parent never calls `wait`. A stale `budget` key in this table is rejected at config load; spend limits belong to the parent.

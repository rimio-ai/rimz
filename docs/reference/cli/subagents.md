# Subagents

`rimz subagents` lets an agent hand one bounded prompt to a supervised child agent and collect the result. The child is a full RimZ agent: its own pane, provider process, durable run record, petname address, and a card nested under its parent in the sidebar. A launch is the same supervised background run as `rimz agents <PROFILE> <PROMPT> -p --bg --timeout 30m`, with the supervision flags chosen for the caller. The [scripting guide](../../guide/scripting.md#agents-scripting-agents) teaches the workflow, and [subagents.md](../../internals/harness/subagents.md) describes the mechanics.

Launching, joining, and stopping need a calling agent that RimZ can identify, through its launch environment or its live process ancestry. The read-only verbs work from any shell:

| Verb | Runs from | Does |
| --- | --- | --- |
| `rimz subagents <PROFILE> <PROMPT>`, `launch` | an agent | [Launch one child](#launch-one-child) |
| `fanout [FILE]` | an agent | [Launch one child per task](#fan-out-a-task-list) in a JSON array |
| `wait <NAME>...` | an agent | [Join](#join-results-with-wait) the named children and print their answers |
| `stop <NAME>...`, `stop --all` | an agent | [Stop](#stop-children) live children |
| `list` (alias `ls`), bare `rimz subagents` | any shell | [List](#list-children) children |
| `profiles` | any shell | [List the profiles](#discover-profiles) a child can launch from |

From a user shell, an agent-only verb fails before it touches the room, and the error points at `rimz subagents list`, `rimz agents`, and `rimz teams`. A child cannot launch anything itself ([Children cannot delegate](#children-cannot-delegate)).

## Launch one child

```sh
rimz subagents claude "trace the authentication call path" --description "trace auth path"
rimz subagents launch reviewer --prompt-file /tmp/review-brief.md
rimz subagents codex "find the smallest safe fix" --wait=10m
```

The bare form and `launch` are the same command. `PROFILE` is a `[subagents.profiles]` profile, an agent kind (`claude`, `codex`, ...), or an `[agents.commands]` command. The launch prints the child's petname on stdout and returns at once, with a receipt on stderr that names the coming fleet report, its response file, and the `wait` command that blocks instead, so a parent can start several children in a row and keep working. A later `rimz subagents wait <petname>` or the [fleet report](#the-fleet-report) delivers the result.

```sh
first=$(rimz subagents codex "find the smallest safe fix")
second=$(rimz subagents reviewer "review the proposed API")
rimz subagents wait "$first" "$second"
```

| Flag | Default | Effect |
| --- | --- | --- |
| `PROMPT` or `--prompt-file <PATH>` | required | The whole assignment. A relative path resolves from the shell's current directory. An empty prompt is refused. |
| `--wait[=DURATION]` | return at launch | Print the petname, then join the child like `subagents wait <name>`. The duration caps the join only; the child keeps its own deadline. Write `--wait=5m`: with a bare `--wait`, a prompt that parses as a duration is refused with that hint. |
| `--json` | off | With `--wait`, print the full run record instead of the petname and answer. Refused without `--wait`. |
| `--timeout <DURATION>` | `[agents.subagents] timeout`, `30m` | Stop the child after this long. Units `s`, `m`, `h`, `d`. |
| `--keep` | off | Hold the pane after the child finishes and after the parent exits, until `rimz subagents stop` closes it. |
| `--isolation host\|sandbox` | the parent's recorded `--isolation` override, else `agents.isolation` | Run the child under this isolation. |
| `--description <TEXT>` | none | Seed the child card's description; the fleet report uses it as the task label. |
| `--model`, `--agent`, `--effort`, `--max-turns`, `-- <ARGS>` | from the profile | Override the model, re-base onto another profile or kind, set reasoning effort, cap agentic turns, or append provider argv. |

Run `rimz subagents launch --help` for the exact spellings. The global flags are on the [CLI page](../cli.md#global-flags).

The complete launch prompt is capped at 120 KiB (122,880 bytes), counting any instruction text RimZ adds to the prompt for providers without an appended system prompt. `--prompt-file` keeps the brief out of the shell command, but the provider still receives its contents as one argument. An oversized prompt is refused before any run record exists, and the error names the limit; put supporting detail in a file the child is told to read.

The child works in the parent's checkout, the directory the parent itself was launched in, whatever directory the launch command runs from. It joins the parent's lane. When that checkout no longer exists, the launch refuses instead of starting the child elsewhere.

A launch refuses before it writes a run record or opens a pane when:

- the caller is not an identifiable agent, or is itself a subagent;
- the profile or `--agent` re-base is outside the caller's [allowlist](#discover-profiles), or names an `[agents.profiles]` profile (the error names both sections);
- the [supervised-run requirements](./agents.md#supervised-runs--p) fail: RimZ's hooks for the agent are not installed and trusted, or a Codex child's checkout has no recorded Codex trust decision;
- the room or provider-account daily cap has no headroom, or a fresh Qwen run's account has an exhausted quota window. This refusal exits `125` ([What a cap blocks](./budget.md#what-a-cap-blocks)).

| Exit | Meaning |
| --- | --- |
| `0` | The child launched. With `--wait`, the child completed. |
| `1` | The launch was refused or failed. With `--wait`, the child failed. |
| `125` | A room or account cap, or a Qwen quota window, refused the launch. With `--wait`, the child's run ended `budget_exceeded`. |
| `123`, `124`, `130` | With `--wait` only: the run's status, or `124` when the `--wait` deadline expired ([exit codes](./agents.md#supervised-runs--p)). |

`rimz subagents` has no `--worktree`, `--from-pr`, `--channel`, `--stdin`, `--resume`, placement flags, output or input formats, `--retries`, or `--verify`. Use `rimz agents` when a launch needs one of those; from an agent, that starts an independent peer that counts against `[agents] max-chain-length`. Use `rimz teams` when the workers are peers rather than children.

## Fan out a task list

```sh
rimz subagents fanout tasks.json
rimz subagents fanout --wait <<'JSON'
[
  {"profile": "codex", "prompt": "review correctness; report concrete findings"},
  {"profile": "claude", "prompt_file": "/tmp/review-brief.md", "description": "review interface"}
]
JSON
```

`fanout` reads a JSON array of tasks from `FILE`, or from stdin when `FILE` is omitted, and launches one child per task. RimZ validates the whole array before the first launch, then opens the panes one at a time; each child starts working as soon as its pane opens. An empty array, an unknown key, or a task without exactly one of `prompt` and `prompt_file` fails the whole list.

| Field | Required | Meaning |
| --- | --- | --- |
| `profile` | yes | Profile, agent kind, or command, as for a single launch |
| `prompt` | one of the two | The assignment |
| `prompt_file` | one of the two | File whose contents become the assignment; a relative path resolves from the current directory |
| `description` | no | Child card description and fleet report task label |
| `timeout` | no | This child's deadline, as a duration string (`"45m"`) |
| `model` | no | Model override |
| `agent` | no | Re-base onto another profile or kind |
| `effort` | no | Reasoning effort |
| `max_turns` | no | Maximum agentic turns |

A task's `timeout` wins over the fanout's `--timeout`, which wins over `[agents.subagents] timeout`. `--keep` applies to every child. Tasks have no isolation, wait, pane-retention, or provider-argv fields, and `fanout` has no `--isolation` flag: every child inherits the parent's isolation, and profiles in `[subagents.profiles]` carry provider arguments. Use separate single launches when children need different lifecycle controls.

| Mode | stdout |
| --- | --- |
| default | Each petname as its child launches; a count and the launch receipt go to stderr |
| `--json` | One object mapping each petname to `{"run_id": "..."}` |
| `--wait[=DURATION]` | Each answer as it finishes, under a `--- <petname> ---` header, with a status suffix only for an abnormal outcome; the exit code follows [`agents wait`](./agents.md#wait) |
| `--wait --json` | The labeled result map of [`agents wait --json`](./agents.md#wait) |

A fanout of one task under `--wait` behaves like a single join: the bare answer without a header, or the full run record with `--json`.

A launch that fails partway stops the remaining launches. The error names the children already started; they keep running under their normal deadline and stay reachable through `subagents wait` and `subagents stop --all`. A cap refusal partway exits `125` with the same message.

## The fleet report

A parent that does not join its children gets one `SUBAGENT_REPORT` message from `@rimz` once every child it launched has settled. A background run it launched with [`rimz agents <kind> -p --bg`](./agents.md#supervised-runs--p) belongs to the same fleet. The message is parked and delivered at the parent's next turn boundary:

```text
Type: SUBAGENT_REPORT
From: @rimz
Content:
All 3 subagents settled:
- @naming: completed in 4m12s, task: "map spec/profile surfaces", response: /tmp/rimz-subagents/naming.output (84 lines)
- @runtime: completed in 5m3s, task: "inspect runtime behavior", no response
- @slow-reviewer: timed out after 30m; provider did not stop, task: "review correctness", response: /tmp/rimz-subagents/slow-reviewer.output (12 lines)
```

The report lists status and where each answer is, and asks for nothing: reading the response files is the parent's call. It never carries a child's answer text; `rimz subagents wait <names>` prints the answers, and `--json` gives structured results.

| Part | Format |
| --- | --- |
| Heading | `Your subagent settled:` for one child, `All {n} subagents settled:` for more; `background agent` replaces `subagent` when any row is a `-p --bg` run |
| Row | `- @{name}: {status} {in\|after} {elapsed}[; {reason}][, task: "{task}"], response: {path} ({N} lines)`, or ending `, no response` |
| Status | `completed`, `failed`, `verify failed`, `timed out`, `budget exceeded`, or `canceled` |
| `in` / `after` | `after` for a timed-out child, `in` for every other status; elapsed time is compact (`4m12s`) |
| Reason | The last non-empty line of the run's failure tail, for a status other than completed |
| Task | The launch `--description`, else a shortened first line of the prompt, omitted when empty |
| Response | The child's final message in `rimz-subagents/<name>.output` under room tmp, with its line count (`1 line`, `{N} lines`, blank lines included); `no response` when the message is empty |

Rows follow launch order. Under sandbox isolation the path reads `/tmp/rimz-subagents/<name>.output`; under host isolation it is the host path of room tmp. The files are removed when the room closes, and opening one does not count as reading the result.

A fleet is every child launched before the report is composed. A child launched while its siblings still run joins that fleet; one launched after composition starts belongs to the next. No report is sent when the parent has ended.

A child drops out of a report that has not been composed yet when:

- a join (`subagents wait`, `fanout --wait`, or `--wait` on a launch) printed its result while the parent's turn was open;
- a [`rimz agents wait`](./agents.md#wait) printed its result from a user shell or while the caller's turn was open, which is how a `-p --bg` run is joined;
- it is a `-p` run the parent launched without `--bg`, which prints its own result;
- the parent stopped it with `rimz subagents stop`.

A join that finishes after the parent's turn has ended still prints, but its rows stay in the report, so the parent is woken with them at its next boundary. A report already queued is canceled only when every row it lists has been read or stopped, and a delivered report cannot be recalled. A child stopped by someone else with `rimz agents stop @child` still appears as `canceled`. If the normal report is missed, the room's sidebar producer rebuilds it from the run records within about a minute ([backstops](../../internals/harness/subagents.md#backstops)).

## Join results with wait

```sh
rimz subagents wait calm-fox
rimz subagents wait calm-fox bright-owl
rimz subagents wait calm-fox bright-owl --any
rimz subagents wait calm-fox --stream
rimz subagents wait calm-fox bright-owl --json --timeout 10m
```

`subagents wait` is [`rimz agents wait`](./agents.md#wait) restricted to the caller's own children: the same output, `--any`, `--stream`, `--timeout`, JSON map, and exit codes. Use it when the next step needs a child's text before the fleet settles, or to reread a result after its pane and response file are gone; results stay readable from the durable run record.

At least one name is required. A bare `wait` fails and lists the caller's children, so a copied command never joins an older fleet by accident. A name that is not one of the caller's children fails with ``` `<name>` is not one of this agent's subagents ```.

## List children

```sh
rimz subagents
rimz subagents list --json
```

Bare `rimz subagents` and `list` are the same read-only command. Which children it shows depends on the caller:

| Caller | Lists | Columns |
| --- | --- | --- |
| an agent | that agent's own children, finished ones included | `SUBAGENT`, `KIND`, `STATUS`, `RUN` |
| a user shell in a channel | every child in the current channel | `SUBAGENT`, `PARENT`, `CHANNEL`, `KIND`, `STATUS`, `RUN` |
| a user shell with no current channel | every child in the room | the same six columns |

`STATUS` is the child's live agent status and `RUN` its newest run's outcome; the description prints as a muted line under each row. Provider-native subagents, which run inside the parent's own process, are not listed.

A plain shell in the project directory has no current channel even when a team runs in place there, because an in-place team's `<directory>/<team>` lane is carried by its panes, not the directory. `list` from that shell shows every channel, and the `CHANNEL` column tells the lanes apart.

`--json` prints an array with one object per child:

| Field | Meaning |
| --- | --- |
| `name`, `handle` | Petname, and the same with `@` |
| `parent` | The parent's handle |
| `channel` | The child's lane; omitted when it has none |
| `kind` | Agent kind |
| `status` | Live agent status |
| `description` | Current one-line description; omitted when empty |
| `run_id`, `run_status` | Newest supervised run and its status; omitted when there is no run |

## Stop children

```sh
rimz subagents stop calm-fox
rimz subagents stop --all
```

`stop` cancels the caller's named live children, or every live child with `--all`, and prints `stopped @<name>` for each. A child that fails to stop prints `error @<name>: <reason>`, and the command exits `1` after trying the rest. With no live child it fails with `this agent has no live subagents to stop`.

Stopping declines the child's result: it leaves the fleet report as described [above](#the-fleet-report). The canceled run stays in the record, so `wait` still reads it. `stop` is also the only way to close a `--keep` pane; `rimz gc` does not reclaim it.

Stopping a parent with `rimz agents stop`, or through `rimz teams stop`, stops its live children first, `--keep` children included.

There is no `restart` or `resume` for a child. To retry, launch the same profile and prompt again. A child is addressable as `@<petname>` for `rimz message` and `rimz pane`, but a supervised child runs one prompt and is not built to read messages mid-run. A message can park against a finished child's address, but nothing resumes the child to read it.

## Discover profiles

```sh
rimz subagents profiles
rimz subagents profiles --path
rimz subagents profiles --json --path
```

`profiles` lists what a child can launch from as compact cards: `[subagents.profiles]` profiles with their agent, model, effort, and description, and `[agents.commands]` commands. It works from any shell. `--path` adds each entry's defining file.

Agent kinds are launchable directly but are not listed. `[agents.profiles]` entries belong to `rimz agents` and are not listed either, and neither are teams, because a launch creates one agent.

`--json` prints an array of objects with `name` and `source` (`profile` or `command`), plus `agent`, `model`, `effort`, and `description` when set, and the absolute `path` only with `--path`.

An `[agents.profiles]` entry can restrict what its agents launch with `subagents = [...]` ([configuration guide](../../guide/configuration.md)). Inside such an agent, `profiles` lists only the named entries, and a launch whose `PROFILE` or `--agent` is not in the list is refused before any run or pane exists. `subagents = []` disables delegation for that agent. An empty catalog, including one disabled this way, prints `No profiles or commands configured.` and an `Add one under [subagents.profiles] or [agents.commands].` line.

At launch, RimZ also gives each agent this same filtered catalog in its appended system prompt, on providers that support one. The catalog says so when delegation is disabled or nothing is configured.

## Children cannot delegate

A child launched through `rimz subagents` is told to complete its assignment itself, and it cannot start further agents:

- RimZ refuses `rimz agents`, `rimz teams`, and `rimz subagents` launches whose caller is a subagent, before any run, pane, or worktree exists.
- The instruction goes into the provider's appended system prompt where the adapter supports one (Claude, Codex, Qwen, Droid), and onto the end of the user prompt otherwise.
- Where the provider has a verified switch, its native delegation tool is disabled: Claude's `Agent` tool is denied, Codex's `multi_agent` feature is turned off, and OpenCode's `task` permission is denied. Other providers get the instruction only.

`[agents] max-chain-length` does not apply to subagent launches, since a child cannot extend the chain. It governs successive peer launches through `rimz agents` and `rimz teams`.

## Panes and the sidebar

Children open in a shared zone instead of splitting the caller's view each time. The first rule that applies places the child:

| Situation | Where the child opens |
| --- | --- |
| The caller's view already has a `<view> subagents` companion tab with room | That companion tab; when every one is full, the next numbered one (`<view> subagents 2`, `3`, ...) |
| The caller is a team member | A new companion tab right after the caller's tab |
| The caller is solo and already has a child beside its pane | Stacked with that child (a native stack on Zellij, equal-height rows on tmux) |
| The caller is solo | A column split to the right of the caller's pane; a companion tab when the split fails |

A companion tab starts with two side-by-side columns and adds rows, keeping pane areas roughly equal, up to eight children (four rows per column, not counting the sidebar). A small terminal or rearranged panes can overflow to the next companion tab sooner. When RimZ cannot read the pane layout, the child opens in an ordinary run tab. A finished child's pane closes itself unless `--keep` is set, and a companion tab closes with its last child pane. The full placement rules are in [pane zones](../../internals/harness/subagents.md#pane-zones).

In the sidebar a child appears only under its direct parent's card, never as a duplicate top-level card, and a finished child stays there until the parent's next prompt. The parent's `⧉ subagents (N)` line counts both launched and provider-native children; the [sidebar page](../../interface/sidebar.md#the-card) describes it. A launched child's spend is added to the parent's all-in figures in the sidebar, `agents show`, teams, and [attribution](./agents.md#attribution).

`rimz transcript @<petname>` reads a child's conversation; channel and `@all` transcript views leave children out ([transcript](./transcript.md)).

## Configure the default timeout

```toml
[agents.subagents]
timeout = "45m"
```

`[agents.subagents]` in `agents.toml` holds one key, `timeout`: the deadline for every child that sets no `--timeout` or task `timeout`, in the duration syntax `s`, `m`, `h`, `d`. It defaults to `30m`. The room enforces the deadline even when no one waits on the child.

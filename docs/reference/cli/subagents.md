# Subagents

`rimz subagents` lets an agent hand one bounded prompt to a supervised child agent and collect the result. The child is a full RimZ agent: its own pane, provider process, durable run record, petname address, and a card nested under its parent in the sidebar. A launch uses the supervised background runner of `rimz agents <PROFILE> <PROMPT> -p --bg`, with a child-specific deadline ladder. The [subagents guide](../../guide/subagents.md) teaches the workflow, and [subagents.md](../../internals/harness/subagents.md) describes the mechanics.

Launching, joining, and stopping need a calling agent that RimZ can identify, through its launch environment or its live process ancestry. The read-only verbs work from any shell:

| Verb | Runs from | Does |
| --- | --- | --- |
| `rimz subagents <PROFILE> <PROMPT>`, `launch` | an agent | [Launch one child](#launch-one-child) |
| `fanout [FILE]` | an agent | [Launch one child per task](#fan-out-a-task-list) in a JSON array |
| `wait <NAME>...` | an agent | [Join](#join-results-with-wait) the named children and print their answers |
| `stop <NAME>...`, `stop --all` | an agent | [Stop](#stop-children) live children |
| `list` (alias `ls`) | any shell | [List](#list-children) children |
| `profiles` | any shell | [List the profiles](#discover-profiles) a child can launch from |

From a user shell, an agent-only verb fails before it touches the room, and the error points at `rimz subagents list`, `rimz agents`, and `rimz teams`. A child cannot launch anything itself ([Children cannot delegate](#children-cannot-delegate)).

## Launch one child

In both the bare and `launch` forms, `PROMPT` may start with `-`; a value that is itself one of the command's flags (`--model`, `--model=…`, `-h`, `--help`, etc.) must instead come from `--prompt-file`, since `--` introduces provider passthrough arguments, not an escaped prompt.

```sh
rimz subagents claude "trace the authentication call path" --description "trace auth path"
rimz subagents launch reviewer --prompt-file /tmp/review-brief.md
rimz subagents codex "find the smallest safe fix" --wait=10m
```

The bare form and `launch` are the same command. `PROFILE` is a `subagents/<name>.md` profile (or trusted project child profile), an agent kind (`claude`, `codex`, ...), or an `[agents.commands]` command. The launch prints the child's petname on stdout and returns at once, with a receipt on stderr that names the coming fleet report, its response file, and the `wait` command that blocks instead, so a parent can start several children in a row and keep working. A later `rimz subagents wait <petname>` or the [fleet report](#the-fleet-report) delivers the result.

The child is supervised until the work ends, not just its first clean turn end. If it arms a wait and ends its turn, its run stays `running` and the fleet report waits too. A lost wake with nothing else left to wake the child fails its run with exit `1` and a reason beginning `parked on a wake that never arrived`; its timeout still applies.

After reporting, an interactive child stays alive until the parent ends the turn that received its result. The parent can send a follow-up with `rimz message @<petname>` in that window; a child turn or pending message delays cleanup. One-shot providers exit after answering. A parent's message to an ended child resumes its session when supported ([follow-ups](#follow-up-messages)). A terminal child awaiting receipt is not bounded by the run timeout; parent loss or an explicit stop reclaims it.

```sh
first=$(rimz subagents codex "find the smallest safe fix")
second=$(rimz subagents reviewer "review the proposed API")
rimz subagents wait "$first" "$second"
```

| Flag | Default | Effect |
| --- | --- | --- |
| `PROMPT` or `--prompt-file <PATH>` | required | The whole assignment. A relative path resolves from the shell's current directory. An empty prompt is refused. |
| `--cwd <DIR>` | parent's checkout | Working directory for the child; the room stays the caller's. Uses the same [path resolution as agent launches](./agents.md#channel-worktree-and-placement). |
| `--wait[=DURATION]` | return at launch | Print the petname, then join the child like `subagents wait <name>`. The duration caps the join only; the child keeps its own deadline. Write `--wait=5m`: with a bare `--wait`, a prompt that parses as a duration is refused with that hint. |
| `--json` | off | With `--wait`, print the full run record instead of the petname and answer. Refused without `--wait`. |
| `--timeout <DURATION>` | `[agents.subagents] timeout`, `30m` | Tell pending or running children to stop at this deadline; kill them `grace` later. Does not bound a terminal child awaiting receipt. Units `s`, `m`, `h`, `d`. |
| `--warn <OFFSETS>` | `[agents.subagents] warn`, `6m,3m` | Comma-separated warning offsets before the deadline; `none` disables warnings. |
| `--grace <DURATION>` | `[agents.subagents] grace`, `3m` | Time to finish reporting after the deadline; `0s` kills at the deadline. |
| `--keep` | off | Disable automatic completion cleanup and cleanup on parent exit; hold the pane after provider exit. Explicit stop and the run timeout still apply. |
| `--detach` | off | Launch without a report: the child stays out of the [fleet report](#the-fleet-report), never holds the parent's turn or run open, and outlives the parent's exit. Its pane closes when it finishes; `subagents wait <name>` still reads the answer. Refused with `--wait`. |
| `--isolation host\|sandbox` | the parent's recorded override, else the child's profile default, else `agents.isolation` | A sandboxed parent refuses a host override; host defaults clamp to sandbox with a note. |
| `--description <TEXT>` | none | Seed the child card's description; the fleet report uses it as the task label. |
| `--model`, `--tier`, `--agent`, `--effort`, `--max-turns`, `-- <ARGS>` | from the profile | Override the concrete model or tier (mutually exclusive), prefer another family or re-base onto another profile or kind, set reasoning effort, cap agentic turns, or append provider argv. |

Run `rimz subagents launch --help` for the exact spellings. The global flags are on the [CLI page](../cli.md#global-flags).

`--tier` and the fanout JSON `tier` field select a [model tier with the same resolution and effort precedence as agents](./agents.md#shared-launch-params). `--model` and JSON `model` accept only concrete models; tier names there are refused. Each task may set `tier` or `model`, not both.

The complete launch prompt is capped at 120 KiB (122,880 bytes), counting any instruction text RimZ adds to the prompt for providers without an appended system prompt. `--prompt-file` keeps the brief out of the shell command, but the provider still receives its contents as one argument. An oversized prompt is refused before any run record exists, and the error names the limit; put supporting detail in a file the child is told to read.

The child works in the parent's checkout, the directory the parent itself was launched in, whatever directory the launch command runs from, unless `--cwd` names another directory. By default it joins the parent's lane; with `--cwd`, its lane follows that directory's basename. When the parent's checkout no longer exists, the launch refuses instead of starting the child elsewhere. Restart and child resume reuse the child's recorded directory.

A launch refuses before it writes a run record or opens a pane when:

- the caller is not an identifiable agent, or is itself a subagent;
- the profile or `--agent` re-base is outside the caller's [allowlist](#discover-profiles), or names a direct profile from `agents/` (the error names both namespaces);
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
| `cwd` | no | Existing working directory, as for `--cwd` on a single launch |
| `prompt` | one of the two | The assignment |
| `prompt_file` | one of the two | File whose contents become the assignment; a relative path resolves from the current directory |
| `description` | no | Child card description and fleet report task label |
| `timeout` | no | This child's deadline, as a duration string (`"45m"`) |
| `warn` | no | Warning offsets as duration strings (`["6m", "3m"]`); `[]` disables warnings |
| `grace` | no | Reporting time after the deadline, as a duration string (`"3m"`); `"0s"` kills at the deadline |
| `model` | no | Model override |
| `tier` | no | `intern`, `junior`, `senior`, or `principal`; conflicts with `model` |
| `agent` | no | Re-base onto another profile or kind |
| `effort` | no | Reasoning effort |
| `max_turns` | no | Maximum agentic turns |

For each of `timeout`, `warn`, and `grace`, the task value wins over the fanout flag, which wins over `[agents.subagents]`. `--keep` and `--detach` apply to every child. Tasks have no isolation, wait, pane-retention, or provider-argv fields, and `fanout` has no `--isolation` flag: every child inherits the parent's recorded override, otherwise follows its own definition then machine policy, capped by the parent's effective isolation. Child definitions carry tool and model settings. Use separate single launches when children need different lifecycle controls.

| Mode | stdout |
| --- | --- |
| default | Each petname as its child launches; the launch receipt goes to stderr |
| `--json` | One object mapping each petname to `{"run_id": "..."}` |
| `--wait[=DURATION]` | Each answer as it finishes, under a `--- <petname> ---` header, with a status suffix only for an abnormal outcome; the exit code follows [`agents wait`](./agents.md#wait) |
| `--wait --json` | The labeled result map of [`agents wait --json`](./agents.md#wait) |

A fanout of one task under `--wait` behaves like a single join: the bare answer without a header, or the full run record with `--json`.

A launch that fails partway stops the remaining launches. The error names the children already started; they keep running under their normal deadline and stay reachable through `subagents wait` and `subagents stop --all`. A cap refusal partway exits `125` with the same message.

## The fleet report

Launcher-opened turns of persistent interactive peers also join this fleet. Every unreported turn retains its own response at `out/<launcher>/<name>.<run_id>.output`, with that turn's prompt as its task and its own elapsed time. Two rows can name the same peer. An open peer run delays the whole fleet report; a user-typed turn without an open peer run does not. These rows use the `background agent` heading and never close the peer's pane.

A child launched with `--detach` is outside the fleet. It adds no row and does not delay the report for the children beside it, the parent is not held open or shown as waiting for it, and its launch receipt says no report will arrive. Its response file is written when it settles, and `subagents wait <name>` joins it on purpose. A follow-up message to a detached child opens an ordinary reported turn; the detached answer itself appears in no report. A detached child that stops on a provider limit sends the parent no [park notice](#a-child-paused-on-a-provider-limit), though `subagents list` still shows it `paused`. A detached child survives its parent's exit, but `subagents stop` and a stop of the parent still stop it, and `agents stop --when-idle` on the parent waits while it works.

A parent that does not join its children gets one `SUBAGENT_REPORT` message from `@rimz` once every child it launched has settled. A background run it launched with [`rimz agents <kind> -p --bg`](./agents.md#supervised-runs--p) belongs to the same fleet; a fleet containing any solo agent run gets `AGENT_REPORT` instead. The message is parked and delivered at the parent's next turn boundary:

```text
Type: SUBAGENT_REPORT
From: @rimz
Content:
All 3 subagents settled, responses total ~5.1k tokens, 96 lines:
- @naming: completed in 4m12s, task: "map spec/profile surfaces", response: /home/me/.rimz/ws/myrepo-f89e/out/planner/naming.output (~4.2k tokens, 84 lines)
- @runtime: completed in 5m3s, task: "inspect runtime behavior", no response
- @slow-reviewer: timed out after 33m; provider did not stop, task: "review correctness", partial response: /home/me/.rimz/ws/myrepo-f89e/out/planner/slow-reviewer.output (<1k tokens, 12 lines)
```

The report lists status and where each answer is, and asks for nothing: reading the response files is the parent's call. It never carries a child's answer text; `rimz subagents wait <names>` prints the answers, and `--json` gives structured results.

| Part | Format |
| --- | --- |
| Heading | `Your subagent settled:` for one child, `All {n} subagents settled:` for more, extended to `All {n} subagents settled, responses total {size}:` when two or more rows carry a response; `background agent` replaces `subagent` when any row is a `-p --bg` run or a peer turn |
| Row | `- @{name}: {status} {in\|after} {elapsed}[; {reason}][, task: "{task}"], response: {path} ({size})`; `partial response:` for a timed-out child with text, or ending `, no response` |
| Status | `completed`, `failed`, `verify failed`, `timed out`, `budget exceeded`, or `canceled`. A child whose CLI exited nonzero before opening a session is relaunched first, three times by default (`[agents] startup-relaunches`), so `failed` then means the last launch died too, and the reason is its output |
| `in` / `after` | `after` for a timed-out answer, `in` for every other status; elapsed time is compact (`4m12s`) and starts at that answer's turn, not the child's launch |
| Reason | The last non-empty line of the answer's failure tail, for a status other than completed |
| Task | The launch answer uses `--description`, falling back to the prompt. Follow-ups use the shortened first line of their own stored prompt, omitted when unknown. Peer turns use their own prompt preview |
| Response | The child's first answer in `out/<parent>/<name>.output` under the room's state directory, then `<name>.2.output`, `<name>.3.output`, and so on for follow-ups; available when that turn settles. `no response` when the message is empty or absent, with no file for that turn; earlier files remain |
| Size | `{tokens} tokens, {lines}`: an estimated token count (`<1k`, `~1.2k`, `~22k`, `~1.2M`) from OpenAI's public `o200k_base` tokenizer, which only approximates Claude's, then the line count (`1 line`, `{N} lines`, blank lines included) |

Rows follow launch order, with a child's earlier answers before its current answer. Each unreported, unjoined answer gets a row; the heading counts rows, so one child can contribute several. Every path is the host path, in every isolation; the host state path is reachable from a sandboxed pane. A subagent's file is available before its wait returns, even while siblings run or if joining it omits it from the report. Closing the room keeps the files; `rimz gc` removes one once it is 7 days old and no run record names its agent, and `rimz reset --hard` removes them all. Opening one does not count as reading the result.

A fleet is every child launched before the report is composed. A child launched while its siblings still run joins that fleet; one launched after composition starts belongs to the next. No report is sent when the parent has ended.

A supervised parent can end its turn while children run without ending its own run. It stays parked while children are live or their results remain unjoined and unreported, and while the report is on its way; the report opens its next turn.

An answer drops out of a report that has not been composed yet when:

- a join (`subagents wait`, `fanout --wait`, or `--wait` on a launch) printed its result while the parent's turn was open;
- a [`rimz agents wait`](./agents.md#wait) printed its result from a user shell or while the caller's turn was open, which is how a `-p --bg` run is joined;
- it is a `-p` run the parent launched without `--bg`, which prints its own result;
- the parent stopped it with `rimz subagents stop`.

A join claims only the answer it prints, leaving earlier unread answers for the report. Stopping a child dismisses all its answers. A join that finishes after the parent's turn has ended still prints, but its rows stay in the report, so the parent is woken with them at its next boundary. A report already queued is canceled only when every answer it lists has been read or stopped, even if a listed child has since answered again; a delivered report cannot be recalled. A child stopped by someone else with `rimz agents stop @child` still appears as `canceled`. If the normal report is missed, the room's sidebar producer rebuilds it from the run records within about a minute ([backstops](../../internals/harness/subagents.md#backstops)).

## Join results with wait

```sh
rimz subagents wait calm-fox
rimz subagents wait calm-fox bright-owl
rimz subagents wait calm-fox bright-owl --any
rimz subagents wait calm-fox --stream
rimz subagents wait calm-fox bright-owl --json --timeout 10m
```

`subagents wait` is [`rimz agents wait`](./agents.md#wait) restricted to the caller's own children: the same output, `--any`, `--stream`, `--timeout`, JSON map, and exit codes. Use it when the next step needs a child's text before the fleet settles, or to reread a result after its pane and response file are gone; results stay readable from the durable run record.

`wait` keeps blocking on a child that is [paused on a provider limit](#a-child-paused-on-a-provider-limit): the run has no answer yet and may still resume. Bound the join with `--timeout`, then read `subagents list`.

At least one name is required. A bare `wait` fails and lists the caller's children, so a copied command never joins an older fleet by accident. A name that is not one of the caller's children fails with ``` `<name>` is not one of this agent's subagents ```.

## List children

```sh
rimz subagents list
rimz subagents list --json
```

`list` is read-only. Bare `rimz subagents`, with no verb and no profile (an empty profile counts as none), refuses from any shell with exit 2 and prints the `list` command to run. Which children it shows depends on the caller:

| Caller | Lists | Columns |
| --- | --- | --- |
| an agent | that agent's own children, finished ones included | `SUBAGENT`, `KIND`, `AGENT`, `RUN`, `ELAPSED` |
| a user shell in a channel | every child in the current channel | `SUBAGENT`, `PARENT`, `CHANNEL`, `KIND`, `AGENT`, `RUN`, `ELAPSED` |
| a user shell with no current channel | every child in the room | the same seven columns |

`AGENT` uses the displayed-status rules of [`rimz agents show`](./agents.md), and `RUN` is the newest run's outcome; the two are independent. A child whose turn stopped on a provider limit, a spend cap, or an overloaded provider reads `paused` while its run stays `running`: the run is still open, and it ends at its [deadline](#configure-the-deadline-ladder) unless the child resumes or the parent stops it. The description prints as a muted line under each row; for a child stopped on a provider error, that line is the provider's message instead (`Usage limit reached`), or the error class in words when the provider gave none. Provider-native subagents, which run inside the parent's own process, are not listed. A child that has not ended reads its provider marker even when its stored status is `failed`; an ended child keeps its settled status.

`ELAPSED` measures the child's current answer: from the newest run's start, or from its latest follow-up prompt when it has one. It counts to now while the run is open and stops at the run's end once it settles; a child with no run shows `-`. Rows list children with an open run first, then settled ones, each newest start first, then children with no run. With no children to list, stdout stays empty and one line on stderr says so, naming `rimz subagents launch` when the caller is an agent; `--json` prints `[]`.

A plain shell in the project directory has no current channel even when a team runs in place there, because an in-place team's `<directory>/<team>` lane is carried by its panes, not the directory. `list` from that shell shows every channel, and the `CHANNEL` column tells the lanes apart.

`--json` prints an array with one object per child:

| Field | Meaning |
| --- | --- |
| `name`, `handle` | Petname, and the same with `@` |
| `parent` | The parent's handle |
| `channel` | The child's lane; omitted when it has none |
| `kind` | Agent kind |
| `status` | Agent status, as `rimz agents list --json` spells it |
| `description` | Current one-line description; omitted when empty |
| `turn_error` | The provider error that explains `status`, with the fields `rimz agents list --json` uses: `class` (`paused_rate_limit`, `paused_spend_limit`, `paused_overloaded`, `unknown`, `failed`) and `label`, the provider's message or `null`; omitted when there is none |
| `run_id`, `run_status` | Newest supervised run and its status; omitted when there is no run |
| `started_at` | RFC 3339 start of the run's current answer, the instant `ELAPSED` counts from; omitted when there is no run |

## A child paused on a provider limit

When a child's turn parks on a provider rate or spend limit and its run remains open, RimZ tells the parent once per stop, with a message parked for the parent's next turn boundary. A reset alone does not guarantee continuation; automatic continuation depends on the configured [resume policy](../../guide/loops.md).

```text
Type: SUBAGENT_PAUSED
From: @rimz
Content:
@still-silver stopped on a provider limit: "Usage limit reached". Its run stays open until its deadline in 24m. Wait for the reset, stop it with `rimz subagents stop @still-silver`, or relaunch the task elsewhere.
```

The quoted text is the provider's own message; when the provider gave none, the notice names the limit class instead (`a rate limit`, `a spend limit`). The deadline clause is omitted for a run with no deadline. A child that resumes and stops on a limit again produces a second notice. A child paused because its provider is overloaded, or parked by a RimZ budget, reads `paused` in `list` but sends no notice.

The notice does not end or change the run, and it is not the fleet report: the report still arrives once every child has settled. The room's sidebar producer detects the stop, so a room with no sidebar running sends no notice. Detection waits for RimZ to record the provider's error and for a producer refresh; delivery then waits for the parent's next eligible turn boundary. There is no fixed delivery delay.

## A child silent past the stall window

A launched child still running but silent for `[agents.attention] stalled_after_secs` (default `1800`) receives a labelled `!` in the sidebar. A child sleeping on a wait, paused, or already showing an error does not trigger this notice. The parent gets one message parked for its next eligible turn boundary:

```text
Type: SUBAGENT_STALLED
From: @rimz
Content:
@still-silver has been silent for 32m and is still running; its pane is open. Its run stays open until its deadline in 24m. RimZ will not stop it: look with `rimz pane capture @still-silver`, stop it with `rimz subagents stop @still-silver`, or relaunch the task elsewhere.
```

The deadline clause is omitted when there is no deadline, or reads `, past its deadline, until it is stopped` after the deadline. The notice is once per run, even if the child recovers and stalls again or a follow-up reopens that run. Fresh activity clears the sidebar's stall label. The notice leaves the run open and does not replace its eventual fleet report. Detection needs the room's sidebar producer; delivery waits for the parent's next eligible boundary. The notice attempt is recorded in [assists](../../internals/harness/loops.md#the-assist-log) and shown by `rimz stats`.

## Stop children

```sh
rimz subagents stop calm-fox
rimz subagents stop --all
```

`stop` cancels the caller's named live children, or every live child with `--all`, and prints `stopped @<name>` for each. A child that fails to stop prints `error @<name>: <reason>`, and the command exits `1` after trying the rest. With no live child it fails with `this agent has no live subagents to stop`.

Stopping declines the child's result: it leaves the fleet report as described [above](#the-fleet-report). The canceled run stays in the record, so `wait` still reads it. `stop` is also the only way to close a `--keep` pane; `rimz gc` does not reclaim it.

Stopping a parent with `rimz agents stop`, or through `rimz teams stop`, stops its live children first, `--keep` children included.

## Follow-up messages

A child is addressable as `@<petname>` for `rimz message` and `rimz pane`. The parent can message a live interactive child for another turn in the same session. If the child has ended, a message from its parent resumes the same session in the subagent zone and delivers the message as its next prompt. Each follow-up keeps its own response file, leaving earlier answers readable at the paths already reported. The next result can produce another fleet report; [`rimz message --wait`](./message.md#wait-for-replies) prints and claims its own reply instead, leaving earlier unread answers for the report.

A provider that registers before its first turn, such as Claude, initially reports the message as `queued` because the child `is resuming`. Delivery follows its registration, with a fallback after 30 seconds by default if registration never arrives. Providers that register lazily, such as Codex, keep immediate delivery.

A message still queued when its child ends is archived rather than left waiting for a vanished pane, and `rimz message show` reports `receiver ended; rimz message @<handle> resumes it`. The parent resends with `rimz message @<handle>`, which resumes the child.

Only the parent can resume a child by message; a user shell, peer or sibling gets a receiver miss. A child stopped explicitly, failed or timed out can still resume. Resume requires its recorded directory, conversation, login, profile and provider resume support to remain usable, and its account to pass the [account check](./agents.md#the-account-check-at-launch); otherwise the miss names the reason. It never silently launches a fresh session. There are no `subagents restart` or `subagents resume` verbs; launch the profile and prompt again when a fresh child is needed.

Room recovery also leaves children closed; [rebirth recovery](../../guide/configuration.md#resume) reports interrupted child runs to the parent instead.

## Discover profiles

```sh
rimz subagents profiles
rimz subagents profiles --path
rimz subagents profiles --json --path
```

`profiles` lists what a child can launch from as compact cards: child definitions with their agent, model, effort, and description, and `[agents.commands]` commands. It works from any shell. `--path` adds each entry's defining file.

Agent kinds are launchable directly but are not listed. Direct profiles belong to `rimz agents` and are not listed either, and neither are teams, because a launch creates one agent.

`--json` prints an array of objects with `name` and `source` (`profile` or `command`), plus `agent`, `model`, `effort`, and `description` when set, and the absolute `path` only with `--path`. Tiered profiles also carry `tier` and `tier_fallback`; cards show the tier beside the concrete model and mark a fallback.

A direct definition can restrict what its agents launch with `subagents: [...]` ([configuration guide](../../guide/configuration.md)). Inside such an agent, `profiles` lists only the named entries, and a launch whose `PROFILE` or `--agent` is not in the list is refused before any run or pane exists. `subagents: []` disables delegation for that agent. An empty catalog, including one disabled this way, prints `No profiles or commands configured.` and a configuration hint.

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

A companion tab starts with two side-by-side columns and adds rows, keeping pane areas roughly equal, up to eight children (four rows per column, not counting the sidebar). A small terminal or rearranged panes can overflow to the next companion tab sooner. When RimZ cannot read the pane layout, the child opens in an ordinary run tab. An interactive child's pane normally closes after the parent's receiving turn ends; one-shot providers close on exit, and `--keep` holds either pane. A companion tab closes with its last child pane. The full placement rules are in [pane zones](../../internals/harness/subagents.md#pane-zones).

In the sidebar a child appears only under its direct parent's card, never as a duplicate top-level card, and a finished child stays there until the parent's next prompt. The parent's `⧉ subagents (N)` line counts both launched and provider-native children; the [sidebar page](../../interface/sidebar.md#the-card) describes it. A launched child's spend is added to the parent's all-in figures in the sidebar, `agents show`, teams, and [attribution](./agents.md#attribution).

`rimz transcript @<petname>` reads a child's conversation; channel and `@all` transcript views leave children out ([transcript](./transcript.md)).

## Configure the deadline ladder

```toml
[agents.subagents]
timeout = "30m"
warn = ["6m", "3m"]
grace = "3m"
```

`[agents.subagents]` in `config.toml` sets the three defaults above. Durations use `s`, `m`, `h`, or `d`; bare `0` is invalid. Warning offsets are sorted largest first and deduplicated; zero offsets and offsets at or beyond the timeout are dropped. Use `warn = []` or `--warn none` for no warnings, and `grace = "0s"` or `--grace 0s` to kill at the deadline as before. The room enforces the kill bound even when no one waits on the child. These settings and flags do not change `rimz agents -p --timeout`.

Claude, Codex, Copilot, Cursor, Droid, Grok, and Qwen receive warning and stop context through post-tool hooks. Qwen's async hook delivers context on the next turn. Other providers receive only the stop instruction through pane steering. A hook-channel child making no tool calls receives no stop context before the kill bound. A late hook gets only the latest crossed warning, with actual remaining time, rounded down to minutes or to seconds below a minute. The last warning asks the child to wrap up; the deadline asks it to stop and report.

At `timeout + grace`, a still-running child becomes `timed_out`. RimZ preserves an existing response or harvests the last non-empty assistant text from its provider transcript. The fleet report labels a harvested result `partial response:`. `subagents wait` prints that text with `rimz: run timed out (exit 124)` on stderr; JSON retains `status: "timed_out"` and `last_message`.

# Subagents

`rimz subagents` lets an agent hand one bounded prompt to a supervised child agent and collect the result. The child is a full RimZ agent: its own pane, provider process, durable run record, petname address, and a card nested under its parent in the sidebar. A launch uses the supervised background runner of `rimz agents <PROFILE> <PROMPT> -p --bg`, with a child-specific deadline ladder. The [subagents guide](../../guide/subagents.md) teaches the workflow, and [subagents.md](../../internals/harness/subagents.md) describes the mechanics.

Launching, joining, and stopping need a calling agent that RimZ can identify, through its launch environment or its live process ancestry. The read-only verbs work from any shell:

| Verb | Runs from | Does |
| --- | --- | --- |
| `rimz subagents <PROFILE> <PROMPT>`, `launch` | an agent | [Launch one child](#launch-one-child) |
| `fanout [FILE]` | an agent | [Launch one child per task](#fan-out-a-task-list) in a JSON array |
| `wait <NAME>...` | an agent | [Join](#join-results-with-wait) the named children: a report row for each settled one, a still-running line for each other |
| `stop <NAME>...`, `stop --all` | an agent | [Stop](#stop-children) live children |
| `list` (alias `ls`) | any shell | [List](#list-children) children |
| `profiles` | any shell | [List the profiles](#discover-profiles) a child can launch from |

From a user shell, an agent-only verb fails before it touches the room, and the error points at `rimz subagents list`, `rimz agents`, and `rimz teams`. To try a subagent profile from your own shell, [`rimz agents <profile> --subagent-profile`](./agents.md#launch-a-subagent-profile) runs it as a top-level agent, not a child. A child cannot launch anything itself ([Children cannot delegate](#children-cannot-delegate)).

## Launch one child

In both the bare and `launch` forms, `PROMPT` may start with `-`; a value that is itself one of the command's flags (`--model`, `--model=…`, `-h`, `--help`, etc.) must instead come from `--prompt-file`, since `--` introduces provider passthrough arguments, not an escaped prompt.

```sh
rimz subagents claude "trace the authentication call path" --description "trace auth path"
rimz subagents launch reviewer --prompt-file /tmp/review-brief.md
rimz subagents codex "find the smallest safe fix" --wait
```

The bare form and `launch` are the same command. `PROFILE` is a `subagents/<name>.md` profile (or trusted project child profile), an agent kind (`claude`, `codex`, ...), or an `[agents.commands]` command. The launch prints the child's petname on stdout and returns at once, with a receipt on stderr that names the coming fleet report, its response file, and the `wait` command that checks on it, so a parent can start several children in a row and keep working. The [fleet report](#the-fleet-report) delivers the result, and `rimz subagents wait <petname>` says where the child stands.

If tier routing moves the child, one stderr warning names its profile, tier, chosen model and every passed entry with its reason, including full build errors. Inspect the recorded tier and passes with [`rimz agents show`](./agents.md#show--inspect). A follow-up that resumes the child prints no new routing warning.

The child is supervised until the work ends, not just its first clean turn end. If it arms a wait or [compacts itself](./agents.md#compact) and ends its turn, or ends it with a message from you or another agent queued for it, its run stays `running` and the fleet report waits too. A lost wake with nothing else left to wake the child fails its run with exit `1` and a reason beginning `parked on a wake that never arrived`; its timeout still applies. A child held only for a message that is then canceled completes with the answer it had. The turn a message opens gets the child's whole `--timeout` again. The report it wrote before parking stays in its answer: the turn that resumes work adds its final message after it, separated by a `---` line, so the response file carries both. This includes a wake turn and a turn in which the child cancels its wait or otherwise works after a cache keepalive.

After its report is queued or joined, an interactive child stays alive until it rests unfocused for `[agents.subagents] linger` (default `10m`, at least `1m`). The parent can send a follow-up with `rimz message @<petname>` in that window; a child turn, a pending message, or pane focus holds cleanup. One-shot providers exit after answering. A parent's message to an ended child resumes its session when supported ([follow-ups](#follow-up-messages)). A lingering terminal child is not bounded by the run timeout; idle linger, parent loss, or an explicit stop reclaims it.

```sh
first=$(rimz subagents codex "find the smallest safe fix")
second=$(rimz subagents reviewer "review the proposed API")
rimz subagents wait "$first" "$second"
```

| Flag | Default | Effect |
| --- | --- | --- |
| `PROMPT` or `--prompt-file <PATH>` | required | The whole assignment. A relative path resolves from the shell's current directory. Trailing newlines in the file are dropped. A blank `PROMPT` or an empty file is refused. |
| `--cwd <DIR>` | parent's checkout | Working directory for the child; the room stays the caller's. Uses the same [path resolution as agent launches](./agents.md#channel-worktree-and-placement). |
| `--add-dir <DIR>` | none | Repeatable additional working directory. Resolved like `--cwd`, canonicalized and deduplicated in flag order. Claude and Codex grant file access without loading its configuration. Antigravity passes it as agy's `--add-dir`, which loads the directory's configuration and runs its hooks, and warns; other kinds warn and launch without the grant. A Codex directory saved untrusted refuses. |
| `--empty-dir` | off | Require a private empty working directory, whatever the profile says. With `--cwd X`, records X as the checkout and keeps its lane; X is not added unless `--add-dir X` is also passed. |
| `--wait[=DURATION]` | return at launch | Print the petname, then join the child like `subagents wait <name>`: its report row once it has settled, else a still-running line and exit `75`. The join returns at once, so a duration has no effect. With a bare `--wait`, a prompt that parses as a duration is refused with a hint to write `--wait=5m`. |
| `--json` | off | With `--wait`, print a one-entry result map keyed by the child's petname instead of the petname and row. The full run record is under `run` in `rimz agents show <ref> --json`. Refused without `--wait`. |
| `--timeout <DURATION>` | the profile's [`timeout:`](../definitions.md), then `[agents.subagents] timeout`, `30m` | Tell pending or running children to stop at this deadline; kill them `grace` later. Does not bound a terminal child lingering after settlement. Units `s`, `m`, `h`, `d`. |
| `--warn <OFFSETS>` | `[agents.subagents] warn`, `10m,5m` | Comma-separated warning offsets before the deadline; `none` disables warnings. |
| `--grace <DURATION>` | `[agents.subagents] grace`, `5m` | Time to finish reporting after the deadline; `0s` kills at the deadline. |
| `--keep` | off | Disable automatic completion cleanup; hold the pane after provider exit until its parent ends or Ctrl-C is pressed in the pane. Explicit stop and the run timeout still apply. |
| `--isolation host\|scoped\|sandbox` | the parent's recorded override, else the child's profile default, else `agents.isolation` | Ordered host < scoped < sandbox: looser explicit overrides refuse; looser defaults clamp to the parent's level with a note. |
| `--yolo` | off | Run the child in yolo mode, up to the parent's own recorded mode. A parent with no recorded mode is treated as auto, so the flag refuses there. Launch the parent with `--yolo` or a `mode: yolo` definition to allow it. |
| `--description <TEXT>` | none | Seed the child card's description; the fleet report uses it as the task label. |
| `--model`, `--tier`, `--agent`, `--effort`, `--fast` or `--no-fast`, `--max-turns`, `-- <ARGS>` | from the profile | Override the concrete model or tier (mutually exclusive), inherit another profile (from `subagents/` or `agents/`) or kind below the child profile, set reasoning effort, turn the provider's [fast mode](../agent-support.md#fast-mode) on or off, cap agentic turns, or append provider argv. |

Run `rimz subagents launch --help` for the exact spellings. The global flags are on the [CLI page](../cli.md#global-flags).

A child's permission mode comes from `--yolo`, its definition or `<kind>-<mode>` cell, otherwise auto. RimZ maps both the request and the parent's recorded ceiling through the [child kind's supported modes](../agent-support.md#permission-modes), then compares plan < ask < auto < yolo; an unrecorded parent counts as auto before mapping. A parent whose profile `args` or `-- <ARGS>` carry raw permission flags records no mode, even beside a declared `mode`, so its children are capped at auto. Pi children always map to yolo and are never clamped or refused on permission mode, because Pi has no permission system. An explicit `--yolo` above the mapped ceiling refuses before any launch is recorded, including OpenCode yolo under an auto parent. A higher default instead lowers to the ceiling with a stderr note naming its source. In-session permission switches do not change the recorded ceiling. A yolo child that resolves to scoped isolation prints a note that sudo and setuid binaries do not work there and names `--isolation host`, unless the parent is itself scoped.

`--tier` and the fanout JSON `tier` field select a [model tier with the same resolution and effort precedence as agents](./agents.md#shared-launch-params), including on a concrete profile, Pi's included. `--model` and JSON `model` build the definition under the model's family on demand (including kinds outside the tier table), without a tier stamp or usage fallback; tier names there are refused. Pi and OpenCode profiles keep their engine under both `--model` and `--tier` (tiers try models in list order, including unowned names), and `--agent pi` or `--agent opencode` also runs other providers' models without switching to their native family. For other engines, an unbuildable family or conflicting `--agent` family refuses with the fix instead of moving the child. Each task may set `tier` or `model`, not both.

The complete launch prompt is capped at 120 KiB (122,880 bytes), counting any instruction text RimZ adds to the prompt for providers without an appended system prompt. `--prompt-file` keeps the brief out of the shell command, but the provider still receives its contents as one argument. An oversized prompt is refused before any run record exists, and the error names the limit; put supporting detail in a file the child is told to read.

The child works in the parent's checkout, whatever directory the launch command runs from, unless `--cwd` names another directory or its profile applies [`empty-dir`](../definitions.md#agent-and-subagent-keys). By default it joins the parent's lane; with `--cwd`, its lane is that directory's own: `main` under the project root, a worktree's name inside it, and elsewhere the directory's basename. When the parent's checkout no longer exists, the launch refuses instead of starting the child elsewhere. Restart and child resume reuse the child's recorded directory.

`--add-dir` does not change that working directory or channel and grants no folder trust. Claude receives file access through its launch settings, not its native added-directory flag, so the added folder's hooks, settings, skills, and instruction files do not load. Codex follows the selected permission mode: read-only and full-access selections need no added-directory flag. Antigravity opens each added folder as an agy workspace, which loads its `AGENTS.md`, rules, and skills and runs its hooks without a trust prompt, so the launch warns. The launch reminder names added directories even when the Environment reminder is off. Restart, resume, and fork reapply them.

`--empty-dir` uses that same private placement even without the profile key or beside `--cwd`. Unlike the profile key, it refuses when the empty root cannot be used, rather than falling back to the checkout. A Codex displaced checkout saved as untrusted still refuses; the explicit flags never route around that decision. Empty-root trust consent and re-entry follow the profile rules below.

For a fresh Claude or Codex child with an explicit `--cwd T` that has no folder-trust decision, RimZ automatically uses `--cwd T --empty-dir --add-dir T`. The child starts privately, keeps T's lane, and can read T without loading its project configuration or instruction files. Stderr prints that reusable command with the named profile and a `"<brief>"` placeholder. When supplied, `--agent <value>` follows the profile unchanged (fanout: the task's `agent` field); no other caller flags are repeated. The reminder names T as `target`, explains why its configuration was not loaded, and asks the child to stop and report if the task needs code run there. The target and added directories are never trusted; only the clean empty root can receive the narrower automatic grant. An unusable empty start refuses with both causes, before a run or pane exists. The reason and directories remain recorded for restart, resume, and fork, and `rimz stats` counts the fallback once the child launches. Inherited undecided directories, Qoder, home-directory launches, and every Codex saved-untrusted directory keep refusing. A Codex projectless directory without a saved decision still starts there normally. An explicit `--empty-dir` takes the requested placement without recording a fallback reason or assist.

An `empty-dir: true` profile starts in its own directory under `~/.rimz/empty/<workspace-dir>/<handle>/`. It keeps the parent's lane and temp directory. On a fresh launch, the profile consents to granting an undecided account's folder trust when its grantable key is exactly the resolved empty root; RimZ prints the account, folder, and changed provider file and records an assist in `rimz stats`. Saved-untrusted, non-grantable, and wider-key accounts start in the parent's checkout with an `empty-dir ignored:` notice pointing to `rimz doctor`; a failed write falls back with its file and error. `--cwd` silently overrides the profile setting. A file or hidden entry beside the workspace or agent directories refuses the launch before any grant and names what to remove. Empty `.agents`, `.aws`, `.codex`, and `.git` directories that a killed Codex Linux sandbox leaves behind do not count, but anything inside one still refuses the launch. Files inside the agent's own directory are its own. Restart and follow-up resume never grant automatically: they recreate the recorded directory if missing, never switch back to the checkout, and do not delete it when the child finishes.

A launch refuses before it writes a run record or opens a pane when:

- the caller is not an identifiable agent, or is itself a subagent;
- the positional profile is outside the caller's [allowlist](#discover-profiles), or names a direct profile from `agents/` (the error names both namespaces);
- the [supervised-run requirements](./agents.md#supervised-runs--p) fail: RimZ's hooks for the agent are not installed and trusted, the child's directory is the home directory (pass `--cwd <DIR>`, or launch the parent from a project directory), or a Claude or Codex child's checkout would show a folder-trust screen (an inherited undecided project or a Codex folder saved as untrusted);
- the room's daily cap or provider account's day or month cap has no headroom, or a fresh Qwen run's account has an exhausted quota window. This refusal exits `125` ([What a cap blocks](./budget.md#what-a-cap-blocks)).

| Exit | Meaning |
| --- | --- |
| `0` | The child launched. With `--wait`, the child completed. |
| `1` | The launch was refused or failed. With `--wait`, the child failed. |
| `2` | Usage error: a flag clap rejects, bare `rimz subagents`, or the retired `--detach`. |
| `125` | A room or account cap, or a Qwen quota window, refused the launch. With `--wait`, the child's run ended `budget_exceeded`. |
| `123`, `124`, `130` | With `--wait` only: the run's status ([exit codes](./agents.md#supervised-runs--p)). |
| `75` | With `--wait` only: the child is still running, and its answer comes in the fleet report, or a prompt for the calling agent was queued ([the message stop](./agents.md#wait)). |

`rimz subagents` has no `--ask`, `--budget`, `--worktree`, `--from-pr`, `--channel`, `--stdin`, `--continue`, placement flags, output or input formats, `--retries`, or `--verify`. `--detach` on `launch` or `fanout` is refused with exit 2, because a subagent ends with its parent; for a run that outlives the caller, launch a peer with `rimz agents <spec> "<prompt>" -p --bg --detach`. Use `rimz agents` when a launch needs one of those; from an agent, that starts an independent peer that counts against `[agents] max-chain-length`. Use `rimz teams` when the workers are peers rather than children.

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
| `add_dirs` | no | Array of additional directory paths, as for repeated `--add-dir`; validated with the whole list before launching any child |
| `empty_dir` | no | Boolean, as for `--empty-dir`; requires a private empty cwd, including beside `cwd` |
| `prompt` | one of the two | The assignment |
| `prompt_file` | one of the two | File whose contents become the assignment; a relative path resolves from the current directory |
| `description` | no | Child card description and fleet report task label |
| `timeout` | no | This child's deadline, as a duration string (`"45m"`) |
| `warn` | no | Warning offsets as duration strings (`["10m", "5m"]`); `[]` disables warnings |
| `grace` | no | Reporting time after the deadline, as a duration string (`"5m"`); `"0s"` kills at the deadline |
| `model` | no | Model override |
| `tier` | no | `intern`, `junior`, `senior`, or `principal`; conflicts with `model` |
| `agent` | no | Inherit another profile (from `subagents/` or `agents/`) or kind below the child profile |
| `effort` | no | Reasoning effort |
| `fast` | no | `true` as `--fast`, `false` as `--no-fast` |
| `max_turns` | no | Maximum agentic turns |

For each of `timeout`, `warn`, and `grace`, the task value wins over the fanout flag, which wins over `[agents.subagents]`; for `timeout` alone, the child definition's `timeout:` sits between the flag and `[agents.subagents]`. `--keep` applies to every child. Tasks have no permission-mode, isolation, wait, pane-retention, or provider-argv fields, and `fanout` has no `--yolo` or `--isolation` flag. Every child's permission default uses the same child-kind-mapped ceiling as a single launch. Isolation inherits the parent's recorded override, otherwise follows the child's definition then machine policy, capped by the parent's effective isolation. Child definitions carry tool and model settings. Use separate single launches when children need different lifecycle controls.

| Mode | stdout |
| --- | --- |
| default | Each petname as its child launches; the launch receipt goes to stderr |
| `--json` | One object mapping each petname to `{"run_id": "..."}` |
| `--wait[=DURATION]` | Each petname as its child launches, then at once, in launch order, a report row for each settled child and a still-running line for each other; the exit code follows [`subagents wait`](#join-results-with-wait) |
| `--wait --json` | The labeled result map of [`subagents wait --json`](#join-results-with-wait) |

A fanout of one task under `--wait` behaves like a single join: the petname, then one row or line, or a one-entry result map with `--json`. For the full run record, use `rimz agents show <ref> --json` and read `run`.

A launch that fails partway stops the remaining launches. The error names the children already started; they keep running under their normal deadline and stay reachable through `subagents wait` and `subagents stop --all`. A cap refusal partway exits `125` with the same message.

## The fleet report

A parent that does not join its children gets one `SUBAGENT_REPORT` message from `@rimz` once every child it launched has settled or is [held on a provider limit](#a-child-paused-on-a-provider-limit). A background run it launched with [`rimz agents <kind> -p --bg`](./agents.md#supervised-runs--p) belongs to the same fleet; a fleet containing any solo agent run gets `AGENT_REPORT` instead. The message is parked and delivered at the parent's next turn boundary:

```text
Type: SUBAGENT_REPORT
From: @rimz
Content:
All 3 subagents settled, responses total ~5.1k tokens, 96 lines:
- @naming: completed in 4m12s, task: "map spec/profile surfaces", response: /home/me/.rimz/ws/myrepo-f89e/out/planner/naming.output (~4.2k tokens, 84 lines)
- @runtime: completed in 5m3s, task: "inspect runtime behavior", no response
- @slow-reviewer: timed out after 33m; provider did not stop, task: "review correctness", partial response: /home/me/.rimz/ws/myrepo-f89e/out/planner/slow-reviewer.output (<1k tokens, 12 lines)
```

The report lists status and where each answer is, and asks for nothing: reading the response files is the parent's call. It never carries a child's answer text, and neither does `rimz subagents wait`, which prints the same rows.

| Part | Format |
| --- | --- |
| Heading | `Your subagent settled:` for one child, `All {n} subagents settled:` for more, extended to `All {n} subagents settled, responses total {size}:` when two or more rows carry a response; with a held line, `{rows} of {rows + held lines} subagents settled:`, extended the same way; `background agent` replaces `subagent` when any row is a `-p --bg` run or a peer turn |
| Row | `- @{name}: {status} {in\|after} {elapsed}[; {reason}][, task: "{task}"], response: {path} ({size})`; `partial response:` for a timed-out child with text, or ending `, no response` |
| Status | `completed`, `failed`, `verify failed`, `timed out`, `budget exceeded`, or `canceled`. Except for configuration refusals (exit 78), a freshly launched child whose CLI exited nonzero before opening a session is relaunched first (a resumed child is not), three times by default (`[agents] startup-relaunches`), so `failed` then means the last launch died too, and the reason is its output. An interrupted turn (Escape in the child's pane) reads `canceled` only once 60 seconds pass with no new turn: until then the child counts as running and holds the report, and a turn started inside that window continues the same run, on the same deadline, with no follow-up row. After the child's deadline stop has gone out, an interrupted turn instead waits for the kill and reads `timed out`. A Claude child's interrupt reaches RimZ at the RimZ mod's report of the turn's end; without the mod it waits for the sidebar's next read of the child's transcript or Claude's next statusline run (the account's `statusLine.refreshInterval` when one is set, else Claude's next render trigger) |
| `in` / `after` | `after` for a timed-out answer, `in` for every other status; elapsed time is compact (`4m12s`) and starts at that answer's turn, not the child's launch |
| Reason | The last non-empty line of the answer's failure tail, for a status other than completed; for an answer that failed on a provider limit, the sentence [below](#a-child-paused-on-a-provider-limit) instead |
| Held line | `- @{name}: paused on a provider limit[: "{label}"]; it refills in {wait} (at {HH:MM} {offset}) and RimZ continues it then. Its report follows`, or `; the limit has refilled and RimZ is continuing it. Its report follows` once the refill has passed; one per held child, after the rows |
| Task | The launch answer uses `--description`, falling back to the prompt. Follow-ups use the shortened first line of their own stored prompt, omitted when unknown. Peer turns use their own prompt preview |
| Response | The child's first answer in `out/<parent>/<name>.output` under the room's state directory, then `<name>.2.output`, `<name>.3.output`, and so on for follow-ups; available when that turn settles. A child that parked on a wait or its own children answers with each turn's final message in order, separated by a `---` line. `no response` when the message is empty or absent, with no file for that turn; earlier files remain |
| Size | `{tokens} tokens, {lines}`: an estimated token count (`<1k`, `~1.2k`, `~22k`, `~1.2M`) from OpenAI's public `o200k_base` tokenizer, which only approximates Claude's, then the line count (`1 line`, `{N} lines`, blank lines included) |

Rows follow launch order, with a child's earlier answers before its current answer. Each unreported, unjoined answer gets a row; the heading counts rows, so one child can contribute several. A held child's line repeats in every report composed while it is held, and a fleet whose only open children are held sends no report until an answer is owed. Every path is the host path, in every isolation; the host state path is reachable from a scoped pane. A subagent's file is available before its wait returns, even while siblings run or if joining it omits it from the report. Closing the room keeps the files; `rimz gc` removes one once it is 7 days old and no run record names its agent, and `rimz reset --hard` removes them all. Opening one does not count as reading the result.

Launcher-opened turns of persistent interactive peers also join this fleet. Every unreported turn retains its own response at `out/<launcher>/<name>.<run_id>.output`, with that turn's prompt as its task and its own elapsed time. Two rows can name the same peer. An open peer run delays the whole fleet report; a user-typed turn without an open peer run does not. These rows use the `background agent` heading and never close the peer's pane.

A fleet is every child launched before the report is composed. A child launched while its siblings still run joins that fleet; one launched after composition starts belongs to the next. No report is sent when the parent has ended.

A supervised parent can end its turn while children run without ending its own run. It stays parked while children are live or their results remain unjoined and unreported, and while the report is on its way; the report opens its next turn.

An answer drops out of a report that has not been composed yet when:

- a join (`subagents wait`, `fanout --wait`, or `--wait` on a launch) printed its row while the parent's turn was open;
- a [`rimz agents wait`](./agents.md#wait) printed its result from a user shell, or its row to its launcher while the launcher's turn was open, which is how a `-p --bg` run is joined;
- it is a `-p` run the parent launched without `--bg`, which prints its own result;
- the parent stopped it with `rimz subagents stop`.

A join claims only the answer its row names, leaving earlier unread answers for the report, and a still-running line claims nothing. Stopping a child dismisses all its answers. A join that finishes after the parent's turn has ended still prints, but its rows stay in the report, so the parent is woken with them at its next boundary. A report already queued is canceled only when every answer it lists has been read or stopped, even if a listed child has since answered again; a delivered report cannot be recalled. A child stopped by someone else with `rimz agents stop @child` still appears as `canceled`. A child canceled because its parent agent ended reads `canceled …; its parent agent ended`, here and in the run status line `rimz subagents wait` prints, whose `--json` entry carries `cancel_reason: "parent_ended"`. If the normal report is missed, the room's sidebar producer rebuilds it from the run records within about a minute ([backstops](../../internals/harness/subagents.md#backstops)).

## Join results with wait

```sh
rimz subagents wait calm-fox
rimz subagents wait calm-fox bright-owl
rimz subagents wait calm-fox bright-owl --any
rimz subagents wait calm-fox bright-owl --json
```

`subagents wait` is [`rimz agents wait`](./agents.md#wait) restricted to the caller's own children, and since every child reports to its parent, it is always the launcher's join, which returns at once. In the order named, it prints a settled child's row in the [fleet report's format](#the-fleet-report) and `- @<name>: still running (<elapsed> so far); it will be in your report` for each other child. It exits with the code `agents wait` gives when every named child has settled, and `75` while any runs; with `--any`, by the first named child that has settled, or `75` when none has. `--timeout` has no effect, `--stream` is refused with a pointer to the report, and there is no `--from-start`. Use it to see where children stand before the fleet settles, or to find a result again after its pane is gone: the row's response file is written again from the durable run record when it is missing.

JSON map keys preserve the reference as typed, including an `@` prefix or lane qualifier. Each entry carries the pet `name` separately. Identical typed references are deduplicated. `--json` prints the same compact keyed map for one target or several: a settled child's entry carries `response_path`, `response_tokens`, and `response_lines` in place of `last_message`, and a running child's entry is `status: "running"` with `exit: 75`. For the full run record, use `rimz agents show <ref> --json` and read `run`.

`wait` reads a child that is [paused on a provider limit](#a-child-paused-on-a-provider-limit) as still running: the run has no answer yet and may still resume, or fail by the pause rule. Read `subagents list` for the pause.

At least one name is required. A bare `wait` fails and lists the caller's children, so a copied command never joins an older fleet by accident; with no children it fails with `this agent has no supervised subagents to wait for`. A name that is not one of the caller's children fails with ``` `<name>` is not one of this agent's subagents ```.

## List children

```sh
rimz subagents list
rimz subagents list --json
rimz subagents list --filter 'status=failed'
rimz subagents list -q
```

`list` is read-only. It reads the room snapshot [`rimz agents list`](./agents.md#list) reads, so it needs a running room and refuses without one with the same message. Bare `rimz subagents`, with no verb and no profile (an empty profile counts as none), refuses from any shell with exit 2 and prints the `list` command to run. Which children it shows depends on the caller:

| Caller | Lists | Columns |
| --- | --- | --- |
| an agent | that agent's own children, finished ones included; with a `--filter` term naming `parent` or `launcher`, what a user shell lists | `SUBAGENT`, `NAME`, `KIND`, `AGENT`, `RUN`, `ELAPSED`; with a term naming `parent` or `launcher`, the eight columns a user shell gets |
| a user shell in a channel | every child in the current channel | `SUBAGENT`, `NAME`, `PARENT`, `CHANNEL`, `KIND`, `AGENT`, `RUN`, `ELAPSED` |
| a user shell with no current channel | every child in the room | the same eight columns |

An agent whose own launch RimZ cannot resolve lists as a user shell does. `SUBAGENT` is the child's bare address, and `NAME` is the child's name written as an address when it differs from that handle, `-` when the two agree.

`--filter` keeps the children matching every term, in the [filter language of `agents list`](./agents.md#filter-the-list), and repeats; `--filter help` prints the fields. The list holds launched children only, whatever the filter: a term narrows it and never adds a root agent, a team seat, or a provider-native child. A term naming `parent` replaces the agent's own-children default, a term naming `launcher` replaces both that default and the current-channel default, and a term naming `lane` replaces the current-channel default (for an agent's own children, a `lane` term only narrows them). `live` has no default, so finished children are listed unless a term such as `live=yes` drops them. A filter the grammar refuses exits 2 and names its term. `-q` prints one address per listed child, in the table's order, and nothing else; each address resolves to its child in the commands that take an address. `-q` conflicts with `--json`.

`AGENT` is the status `rimz agents list --filter 'parent=*'` shows for the same child, and `RUN` is the newest run's outcome; the two are independent. A child whose turn parks on a provider rate/spend limit or overload reads `paused`. Its run stays `running` while the stopped turn is left open, or when an errored turn end carries a rate limit, spend limit, or overload marker; an errored turn end with any other error fails the run at once, so `RUN` reads `failed`. An open overload park stays open until its [deadline](#configure-the-deadline-ladder) unless the child resumes or the parent stops it; an open rate or spend limit is held to its refill or fails, by the [pause rule](#a-child-paused-on-a-provider-limit). Once the park can no longer resume (auto-continue retries spent, or a limit window reset with no spent window left), `AGENT` reads `failed`. `RUN` then still says `running` for an overload park, until its deadline; for a held rate or spend park while auto-continue is on with retries left, until its moved deadline; and for a rate or spend park the pause rule never classified (a child reporting to no one, no sidebar running, or a parent that had already ended), until its deadline. Any other rate or spend park's run is failed by the pause rule, a held one on the first producer refresh after the later of its refill and 10 minutes from the stop, so `RUN` reads `failed` too. The Claude Console API usage cap (`You have reached your specified API usage limits`) is not a park: `AGENT` reads `failed`, not `paused`, and the run fails at the error, so `RUN` reads `failed` too. A child [silent past the stall window](#a-child-silent-past-the-stall-window) reads `failed` while the room shows it nested under its parent's card; a child with no nested card (its parent's card is gone, or the room has dropped the child's own stale row) keeps its own status, `running` for a silent turn. The description prints as a muted line under each row; for a child reading `failed` for its silence, that line is the silence (`silent 17m`), and for a child stopped on a provider error, the provider's message (`Usage limit reached`), or the error class in words when the provider gave none. For a child whose run was canceled because its parent agent ended, it reads `canceled: its parent agent ended`. Provider-native subagents, which run inside the parent's own process, are not listed. A child that has not ended reads its provider marker even when its stored status is `failed`; an ended child keeps its settled status.

`ELAPSED` measures the child's current answer: from the newest run's start, or from its latest follow-up prompt when it has one. It counts to now while the run is open and stops at the run's end once it settles; a child with no run shows `-`. Rows list children with an open run first, then settled ones, each newest start first, then children with no run. With no children to list, stdout stays empty and one line on stderr says so, naming `rimz subagents launch` when the caller is an agent; with a filter given, the line says no subagent matches the filter instead. `--json` prints `[]`, and `-q` prints nothing.

A plain shell in the project directory has no current channel even when a team runs in place there, because an in-place team's lane, named after the team, is carried by its panes, not the directory. `list` from that shell shows every channel, and the `CHANNEL` column tells the lanes apart.

`--json` prints a bare array with one object per child. Each object carries the 19 keys of the [`agents list --json` field table](./agents.md#filter-the-list), with `null` for a missing value, and beside them the keys below; where both name a key, the field table's value is the one printed:

| Field | Meaning |
| --- | --- |
| `name`, `handle` | Petname (`null` when the child has none), and the shortest address that resolves back to the child, which can carry `#lane` |
| `parent` | The parent's address, as `agents list` prints it; `null` when the parent's row is gone |
| `channel` | The child's lane; omitted when it has none |
| `kind` | Agent kind |
| `status` | The `AGENT` status, as `rimz agents list --json` prints it for the same child |
| `description` | Current one-line description; omitted when empty |
| `turn_error` | The provider error that explains `status`, with the fields `rimz agents list --json` uses: `class` (`paused_rate_limit`, `paused_spend_limit`, `paused_overloaded`, `unknown`, `not_entitled`, `failed`) and `label`, the provider's message or `null`; omitted when there is none. The flat `error` key carries the same class in the filter's spelling (`rate-limit`) |
| `run_id`, `run_status` | Newest supervised run and its status; omitted when there is no run |
| `cancel_reason` | Why RimZ canceled the newest run: `parent_ended` when the child's parent agent ended first; omitted otherwise, a stop included |
| `started_at` | RFC 3339 start of the run's current answer, the instant `ELAPSED` counts from; omitted when there is no run |

## A child paused on a provider limit

When a child's turn parks on a provider rate or spend limit and its run remains open, RimZ reads when the limit refills from the provider usage it last recorded for the child's account and model, and either holds the run until then or fails it:

| Refill | The run |
| --- | --- |
| known, beyond the hold limit | fails at once |
| known, within the hold limit, auto-continue on with retries left | stays open; its deadline and its next warning move later by the time from the stop to the refill |
| any other case | waits 10 minutes after the stop, then fails, unless the child acts or a refill within the hold limit is read while auto-continue is on |

The hold limit is `harness.cache_keepalive_max` (default `6h`; `6h` too when it is `off`), whether or not cache pings are on. An Antigravity child's quota stop is held the same way, with the refill read from the account's quota windows rather than from the refill the stop names. A held run that reaches its refill is continued by [auto-continue](../../guide/loops.md#auto-continue) and stays open on its moved deadline. When auto-continue has been turned off or has spent its retries on this park, a held run fails at the later of its refill and 10 minutes after the stop. A child that resumes before its refill gets back the unused part of the hold: its deadline moves earlier by the time left until the refill. A hold sends no message of its own: a held child does not hold back the [fleet report](#the-fleet-report) of its siblings, which carries a line for it, and when a sibling's answer is already waiting, the hold sends that report at once.

A failed run keeps its typed cause (`limit_failure` on the run record, under `run` in `rimz agents show <ref> --json`) and a reason naming the provider's message and the refill when known, stored as the run's failure tail:

```text
stopped on a provider limit: "Usage limit reached"; it refills at 2026-11-11 04:00:00 +01:00
```

A failure whose continue after the refill failed reads `stopped on a provider limit: "Usage limit reached"; continuing it after the refill failed`, and one whose refill RimZ has not read ends after the provider's message. When the provider gave no message, the quoted part is left out.

The fleet report computes its row's reason when it is composed, from the typed cause and from the account a message to the child would resume it on (the room's current account for the child's kind):

```text
- @still-silver: failed in 12m; stopped on a provider limit: "Usage limit reached"; it refills in 2h10m (at 21:00 +01:00). Message it after the refill to continue it, task: "review correctness", no response
```

| Case | The reason ends |
| --- | --- |
| the room's account cannot read the child's session | `. The room's codex account has changed and cannot read its session: do the task yourself` |
| another account, no refill ahead on it | `. The room's codex account has changed since it stopped: message it to retry now; if that fails too, do the task yourself` |
| another account, its refill ahead | `; it refills in 2h10m (at 21:00 +01:00). Message it after the refill to continue it` |
| same account, the continue after the refill failed | `, and continuing it after the refill failed. Message it to retry; if that fails too, do the task yourself` |
| same account, refill beyond the hold limit and still ahead | `; it refills in 31h (at 04:00 +01:00 tomorrow). Do the task yourself; a message after the refill continues it` |
| same account, refill within the hold limit and still ahead | `; it refills in 2h10m (at 21:00 +01:00). Message it after the refill to continue it` |
| same account, quota read available, or the refill passed | `; the limit has refilled. Message it to continue it` |
| same account, refill unknown | `. Message it to retry; if that fails too, do the task yourself` |

`codex` names the child's kind. A wait is hours and minutes rounded up to the minute (`48m`, `3h05m`), whole hours from 24 hours on (`31h`); the clock is local, with ` tomorrow` or ` on {date}` for a later day. An account the room cannot resolve counts as the same account. A run with a failure tail but no typed cause, as an older RimZ wrote it, gets its stored sentence. A park's failed run has its pane reclaimed as for a [timeout](#configure-the-deadline-ladder). A child that resumes and stops on a limit again is held or failed afresh. A child paused because its provider is overloaded, or parked by a RimZ budget, reads `paused` in `list` and is neither held nor failed.

The room's sidebar producer detects a park, so a room with no sidebar running neither holds nor fails a parked child. Detection waits for RimZ to record the provider's error and for a producer refresh.

## A child silent past the stall window

Any child still running but silent for `[agents.attention] stalled_after_secs` (default `900`) receives a labelled `!` in the sidebar. The same window covers a launched child given a task that has not started its first turn, whether or not it has registered. A promptless interactive child waiting for a human is excluded. Provider-native children lift the parent card's attention but send no notice. A child sleeping on a wait, paused, or already showing an error does not trigger the silence check. A launched child stopped on a turn error that nothing will retry does, after the same window of silence: one showing `failed` (an auto-continue park whose retries are spent included), or one paused on an overloaded provider while auto-continue is off. A child paused on a rate or spend limit is left to the [limit hold or failure](#a-child-paused-on-a-provider-limit), and one paused on an overload with auto-continue retries left to auto-continue. For a launched child, the parent gets one message parked for its next eligible turn boundary:

```text
Type: SUBAGENT_STALLED
From: @rimz
Content:
@still-silver has been silent for 17m and is still running; its pane is open. Its run stays open until its deadline in 13m. RimZ will not stop it: look with `rimz pane capture @still-silver`, stop it with `rimz subagents stop @still-silver`, or relaunch the task elsewhere.
```

For a never-started child, the opening instead reads `@still-silver has not started its first turn for 17m; its pane is open.` For a stopped child, the notice quotes the reason the child shows and offers continuing it:

```text
@still-silver stopped on a turn error 17m ago: "Auto mode is unavailable: the server returned no safety verdict for 10 responses"; its pane is open. Its run stays open until its deadline in 13m. RimZ will not continue it: look with `rimz pane capture @still-silver`, continue it with `rimz message @still-silver "continue"`, stop it with `rimz subagents stop @still-silver`, or relaunch the task elsewhere.
```

The deadline clause is omitted when there is no deadline, or reads `, past its deadline, until it is stopped` after the deadline. The notice is once per run, even if the child recovers and stalls again or a follow-up reopens that run. Fresh activity clears the sidebar's stall label. The notice leaves the run open and does not replace its eventual fleet report. Detection needs the room's sidebar producer; delivery waits for the parent's next eligible boundary. If the child's run ends first, the notice is withdrawn and reads `canceled` in `rimz message show <id>`, with the reason in its timeline. The notice attempt is recorded in [assists](../../internals/harness/loops.md#the-assist-log) and shown by `rimz stats`.

## Stop children

```sh
rimz subagents stop calm-fox
rimz subagents stop --all
```

`stop` cancels the caller's named live children, or every live child with `--all`, and prints `stopped <address>` for each, with the child's name beside its address when the two differ (`stopped @coder (@round-binder)`). A child that fails to stop prints `error <address>: <reason>` the same way, and the command exits `1` after trying the rest. With no live child it fails with `this agent has no live subagents to stop`.

After closing the whole set, `stop` waits up to 3 seconds total for the closed agents' processes to exit, sends SIGTERM once to each closed agent's process still running, and waits up to 3 seconds more. A child counts as stopped when no pane hosts it and its process is gone, or, for a process RimZ does not know, its end is recorded; any other child is reported with its reason (the pane still hosting it, or the pid still running) and the command exits 1.

Stopping declines the child's result: it leaves the fleet report as described [above](#the-fleet-report). The canceled run stays in the record, so `wait` still reads it. A `--keep` pane also closes when its parent ends or Ctrl-C is pressed in the pane; Ctrl-C does not decline the answer. `rimz gc` does not reclaim it.

A successful stop ends the child's card even if its pane was already gone, so a later [parent message](#follow-up-messages) resumes it or names why it cannot resume. If the stop named a still-running process, wait for it to exit before resuming.

Stopping a parent with `rimz agents stop`, or through `rimz teams stop`, stops its live children first, `--keep` children included.

## Follow-up messages

A child is addressable as `@<petname>` for `rimz message` and `rimz pane`. The parent can message a live interactive child for another turn in the same session. If the child has ended, a message from its parent resumes the same session in the subagent zone and delivers the message as its next prompt. To reopen an ended child in its live parent's subagent zone without queuing a message, run [`rimz agents resume @<petname>`](./agents.md#resume-one-closed-session) (or pass its session id); if the child is already live, `rimz agents resume` focuses its pane instead. Each follow-up keeps its own response file, leaving earlier answers readable at the paths already reported. The next result can produce another fleet report.

An ended child's follow-up is queued before its pane opens, for every provider. The receipt names its message id, says the child is resuming, says when the follow-up is typed for the child's kind, and gives an absolute UTC deadline: `delivers once its provider starts a session; fails if none has started by` for a kind that waits for that start, and `delivers once its pane comes up` (lazy kinds) or `delivers one delivery window after its pane comes up` (every other kind), each followed by `fails if it has not come up by`. Nothing is typed into the resumed pane before the provider reports its own session start, for each kind whose resume reports that start before its first prompt: Claude, Amp, Grok, Kimi and Pi, or a plugin that declares `reports-start-on-resume`. A kind that registers lazily at its first prompt (Codex, Kiro, OpenCode, MiniMax, Antigravity, Muse, or a plugin that declares it) gets the follow-up at once. Every other kind (Copilot, Cursor, Droid, Hermes, OMP, Qoder, Qwen, and any other plugin) gets it once one delivery window has passed since the pane opened or its provider reported earlier. The three-second pane-bind check does not discard the follow-up. Another follow-up during that registration wait queues behind it and shares its deadline without opening a second pane or extending the wait. Steer and interrupt to an ended child also become its next prompt. If opening the pane fails, the message is errored and the command reports the opening error.

The registration limit is two minutes from the follow-up that opens the pane (`RIMZ_MESSAGE_REGISTER_TIMEOUT_MS` overrides it). At or after that deadline, the room's sweep expires a still-queued follow-up only if the child's card is absent, ended no later than enqueue, or resumed with no session started by a provider that reports its start on resume (Claude, Amp, Grok, Kimi and Pi, or a declaring plugin). In that last case the messages queued behind it expire too, the supervisor releases the child's pane, and the reason reads `@<handle>'s resumed <kind> did not start a session by <UTC time>; launch a fresh child with rimz subagents <profile> <task>`, with `<profile>` left for the sender to fill in with what it launches. A child that has registered but is waiting at a provider prompt does not expire; a child that ends after enqueue follows the archival rule below instead. `rimz message show <id>` explains the registration wait and its deadline, then the expiry reason if it fails. An agent sender gets the ordinary undelivered notice. Without a sidebar host or an explicit sweep/gc, the record stays queued, but the next follow-up after the deadline still opens a new pane.

A message still queued when its child ends is archived rather than left waiting for a vanished pane, and `rimz message show` reports `receiver ended; rimz message @<handle> resumes it`. The parent resends with `rimz message @<handle>`, which resumes the child. A follow-up queued after that end waits for resume registration instead. A resumed provider that exits before it takes its follow-up archives the follow-up and everything queued behind it with `@<handle>'s resumed <kind> exited with code <n> before it took the follow-up; launch a fresh child with rimz subagents <profile> <task>` (`was killed` for a signal), since a resend would resume the same session again.

A resumed child whose follow-up fails before starting its next turn is cleaned up by its supervisor. Its earlier reported answer remains unchanged and is not reported again.

Only the parent can resume a child by message; a user shell, peer or sibling gets a receiver miss. A child stopped explicitly, failed or timed out can still resume. Resume requires its recorded directory (recreated when an empty-dir child's directory is missing), conversation, login, profile and provider resume support to remain usable, and its account to pass the [account check](./agents.md#the-account-check-at-launch); otherwise the miss names the reason. A child whose run failed on a [provider limit](#a-child-paused-on-a-provider-limit), and that passes every other resume check, is not resumed while the account the resume would use still reads a refill ahead, or, on the child's own account, while the refill the failure recorded is ahead and neither a usage read newer than the failure shows quota available nor the account has switched since: the message exits 1 with ``not sent: @still-silver's provider limit refills in 1h48m (at 21:00 +01:00). Run `rimz wait --in 108m`, then send it again.``, queues nothing, opens no pane, and is recorded as an unresolved message with that reason. The `--in` duration is whole minutes, rounded up. A message after the refill resumes the child. A limit-failed child that cannot resume for another reason gets that reason at once, whatever its refill. A replayed permission mode above the parent's current recorded ceiling (unrecorded is auto), after mapping both through the child kind, refuses with both modes and asks for a new child rather than lowering the resumed session's grant. Pi resumes never refuse on permission mode. It never silently launches a fresh session. There are no `subagents restart` or `subagents resume` verbs; launch the profile and prompt again when a fresh child is needed.

Room recovery also leaves children closed; [rebirth recovery](../../guide/configuration.md#resume) reports interrupted child runs to the parent instead.

## Discover profiles

```sh
rimz subagents profiles
rimz subagents profiles --path
rimz subagents profiles --json --path
```

`profiles` lists what a child can launch from as compact cards: child definitions with their agent, model, effort, and description, and `[agents.commands]` commands. Each first line ends with the [origin label](../definitions.md#validation-and-failures) in brackets, including `[project]` for a trusted project's profile and `[mine → crew]` for a home definition inheriting from pack `crew`. It works from any shell. `--path` adds each entry's defining file.

A [kind base](../definitions.md#kind-bases) (`agents/<kind>.md`) is listed under its kind's name, since a child can launch from it. An agent kind with no home or pack kind base is launchable directly but is not listed. Direct profiles belong to `rimz agents` and are not listed either, and neither are teams, because a launch creates one agent.

`--json` prints an array of objects with `name`, `source` (`profile` or `command`), and `origin` (`mine`, `pack`, or `project`), plus `agent`, `model`, `effort`, `description`, and `allowed_tools` (an array of permission rules) when set, `fast: true` only when the profile sets fast mode, and the absolute `path` only with `--path`. A card with a nonempty permission-rule list adds an `allowed-tools:` line after the description. `pack` is present only for a pack origin; `inherits` is present only for a definition's own qualified `agent:` parent. Tiered profiles also carry `tier` and `tier_fallback`; cards show the tier beside the concrete model and mark a fallback.

A direct definition can restrict what its agents launch with `subagents: [...]` ([configuration guide](../../guide/configuration.md)). Inside such an agent, `profiles` lists only the named entries (resolved in the caller's launch chain order under any recorded `--agent`), and a launch whose `PROFILE` is not in the list is refused before any run or pane exists; `--agent` on an allowed `PROFILE` is not checked against the list. `subagents: []` disables delegation for that agent. An empty catalog, including one disabled this way, prints `No profiles or commands configured.` and a configuration hint.

At launch, RimZ also gives each non-child launch except a workflow judge this same filtered catalog in its appended system prompt, on providers that support one, when its [skill policy](../../guide/configuration.md#skills) makes `rimz-subagents` callable or the launch serves the `subagent` MCP tool. A profile with `subagents: []` is neither served the tool nor offered the skill, so it gets no catalog paragraph; when no entries are available, the paragraph says no subagent profile is configured for the agent and tells it to work directly. Without callable `rimz-subagents` or the served `subagent` MCP tool, there is no catalog paragraph.

## Children cannot delegate

A child launched through `rimz subagents` is told to complete its assignment itself, and it cannot start further agents:

- RimZ refuses `rimz agents`, `rimz teams`, and `rimz subagents` launches whose caller is a subagent, before any run, pane, or worktree exists.
- The child's launch serves neither the `rimz-subagents` nor the `rimz-loop` capability of RimZ's [MCP server](./mcp.md), and the `schedule` tool refuses a booking whose caller is a subagent.
- The instruction goes into the provider's appended system prompt where the adapter supports one (including Qoder), and onto the end of the user prompt otherwise.
- The child's skill view withholds model invocation of `rimz-subagents`, `rimz-agents`, and `rimz-teams` where skill restrictions are supported, whether or not the profile configures a list. Other skills keep the profile's policy ([skill views](../../internals/sandbox.md#profile-skill-views)).
- Where the provider has a verified switch, its native delegation tool is disabled: Claude's `Agent` tool is denied, Codex's native agents are disabled, and OpenCode's built-in subagent tool is removed. Other providers keep their native tools. The same native switch applies to parents offered RimZ delegation: the callable `rimz-subagents` skill or the served `subagent` MCP tool, plus an available catalog, even when `tools:` lists `Agent`; with neither, a parent's tools stay as its tools list renders them.

`[agents] max-chain-length` does not apply to subagent launches, since a child cannot extend the chain. It governs successive peer launches through `rimz agents` and `rimz teams`; [`rimz agents molt`](./agents.md#molt) replaces the caller at the same generation instead. Subagents cannot molt.

## Panes and the sidebar

Children open in a shared zone instead of splitting the caller's view each time. The first rule that applies places the child:

| Situation | Where the child opens |
| --- | --- |
| The channel already has a `#<channel> subagents` companion tab with room | That companion tab; when every one is full, the next numbered one (`#<channel> subagents 2`, `3`, ...) |
| The caller is a team member | A new companion tab right after the caller's tab |
| The caller is solo and already has a child beside its pane | Stacked with that child; focus opens a member on either backend |
| The caller is solo | A column to the right of the caller's pane, or its whole stack when it is a member, on either backend; a companion tab when the split fails |

A companion tab holds one collapsing stack; jumping to a child from the sidebar expands its member. New children reuse the first numbered companion whose stack can keep the expanded member at least 10 content rows, then overflow to the next tab. Pre-upgrade grids and manually split companions take no new children and drain in place. On Zellij below 0.45, each child after the first needs its own numbered tab to preserve focus. When RimZ cannot resolve placement, the child still opens in a `#<channel> subagents` tab. On the main channel the companion base is `<view> subagents`, or `subagents` when the caller's view is unavailable. An interactive child's pane stays open for `[agents.subagents] linger` (default `10m`) after its report is queued or joined while it rests unfocused; one-shot providers close on exit, and `--keep` holds either pane until parent end or Ctrl-C in the pane. A companion tab closes with its last child pane. The full placement rules are in [pane zones](../../internals/harness/subagents.md#pane-zones).

In the sidebar a child appears only under its direct parent's card, never as a duplicate top-level card, and a finished launched child stays on the card while its record is kept, folded under `N earlier` at the parent's next prompt. The parent's `⧉ subagents (N)` line counts both launched and provider-native children of the parent's current turn; the [sidebar page](../../interface/sidebar.md#the-card) describes it. A launched child's spend is added to the parent's all-in figures in the sidebar, `agents show`, teams, and [attribution](./agents.md#attribution).

`rimz transcript @<petname>` reads a child's conversation; channel and `@all` transcript views leave children out ([transcript](./transcript.md)).

## Configure the deadline ladder

```toml
[agents.subagents]
timeout = "30m"
warn = ["10m", "5m"]
grace = "5m"
linger = "10m"
```

`[agents.subagents]` in `config.toml` sets the four defaults above. A subagent definition's [`timeout:`](../definitions.md) replaces the timeout default for its children; warn and grace come only from the launch or this table, and `linger` comes only from this table. A duration is one whole number and one unit, `s`, `m`, `h`, or `d` (`90m`, not `1h30m`); bare `0` is invalid. Warning offsets are sorted largest first and deduplicated; zero offsets and offsets at or beyond the final timeout, whichever level set it, are dropped. Use `warn = []` or `--warn none` for no warnings, and `grace = "0s"` or `--grace 0s` to kill at the deadline. `linger` (default `10m`, at least `1m`) is the idle grace after a finished interactive child's report is queued or joined before its wrapper closes the pane; a focused pane holds the close without resetting the clock, and edits to `linger` reach children already lingering. The room enforces the kill bound even when no one waits on the child. These settings and flags do not change `rimz agents -p --timeout`.

Claude, Codex, Copilot, Cursor, Droid, Grok, OpenCode, Pi, and Qwen receive warning and stop context through post-tool hooks. Qwen's async hook delivers context on the next turn. OpenCode's plugin adds it to the system text of each model request after the tool call, until the turn ends. Antigravity receives warnings, not the stop, as a system message before its next model call; Gemini models act on it, while Claude models under Antigravity read it and do not. Every child also gets the stop as an interrupt in its pane: its interrupt key, a wait for the turn to stop, then the stop typed in, so a child stuck in one long command still has the rest of the grace to report. A child with hook context other than Antigravity gets it when it makes no tool call within 10 seconds of the deadline (half the grace when the grace is under 20 seconds), and any other child at the deadline. A child waiting on your answer in its pane, or whose permission is under automatic review, gets no interrupt until it is answered; with hook context other than Antigravity, its stop arrives through the hook on its first tool call after the answer, and an Antigravity child gets the interrupt once answered. A typed stop held while you type in the child's pane waits in the queue, and is withdrawn if the child's run ends, or ends and reopens with a follow-up, before it is typed. A late hook gets only the latest crossed warning, with actual remaining time, rounded down to minutes or to seconds below a minute. The last warning asks the child to wrap up; the deadline asks it to stop and report.

At `timeout + grace`, a still-running child becomes `timed_out`. RimZ preserves an existing response or harvests the last non-empty assistant text from its provider transcript. The fleet report labels a harvested result `partial response:`. `subagents wait` prints its `partial response:` row and exits `124`; its JSON entry keeps `status: "timed_out"` and names the response file.

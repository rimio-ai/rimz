# Watch CLI

`rimz watch` is an alarm an agent sets for itself on a state outside its shell: CI, a pull request, a review, a team stage, or a usage window. The agent arms a watch and ends its turn or goes on with other work, and RimZ sends a message back into the same conversation once the expression reads true. Only an agent RimZ can identify (from its launch environment or process ancestry) can arm or cancel a watch, and every watch wakes that agent's own live session. To wait on a command, a log, a process, or a delay, run it with [`rimz shell`](./shell.md).

Three other commands sit near this one. [`rimz agents wait`](./agents.md#list-and-manage-agents) blocks a shell until an agent finishes. A team wakes its members on a signal through [role bindings](./teams.md#signal-bindings-at-launch). A [loop task](./loop.md#add-a-task) runs a command, such as `rimz message`, on a clock, a signal, or a condition. The [loops guide](../../guide/loops.md#wake-a-running-agent) teaches when to use each.

```sh
rimz watch pr=merged                              # your checkout's pull request merges
rimz watch ci=passed,failed                       # your checkout's CI settles
rimz watch 'window.5h.left>=50' --label refactor  # the usage window has room again
rimz watch --check-in 1h team.stage=Review        # one check-in after 1h
rimz watch list                                   # your watches, or the whole room's from a user shell
rimz watch cancel watch-mint-spring
rimz watch cancel --all
```

The [global flags](../cli.md#global-flags) apply.

## Arm a watch

The expression is one positional argument: quote it when it holds a shell character such as `>`, `&&`, `||`, `!`, or a space. With no expression and no option but `--json`, `rimz watch` lists your watches, and `rimz watch --json` is `rimz watch list --json`.

| Flag | Meaning |
| --- | --- |
| `--label <TEXT>` | Short description: one line, 1 to 60 characters after trimming. It follows the expression as ` · <label>` in lists and the message. |
| `--check-in <DURATION>` | When to send one check-in. Greater than zero and less than `24h`. The default is in [check-ins](#check-ins---check-in). |
| `--timeout <DURATION>` | Stop watching once this long has passed with the expression false, at most `365d`. One message names the timeout: `timed out after <ELAPSED>`. |
| `--json` | Print the receipt as JSON. |

A duration is one whole number and one unit, `s`, `m`, `h`, or `d` ([durations](../cli.md#durations)). A compound value such as `1h30m` is an argument error.

Each arm mints a name of the form `watch-<adjective>-<noun>`, unique in the workspace, with a numeric suffix in a crowded namespace. The receipt names the watch, `when <expression>`, and the target, adds a `now:` line, then prints your [list](#list-your-watches):

```console
$ rimz watch --label 'review it' team.stage=Review
armed watch-mint-spring: when team.stage=Review → @coder#probe
  now: waiting · team.stage: Build
@coder#probe (@smart-beacon) · 1 watch
  watch-mint-spring  waiting  when team.stage=Review · review it
```

The receipt ends with `(check-in <DURATION>)` when the watch records a check-in. The one above has none, because cache keepalive covers that agent ([check-ins](#check-ins---check-in)).

The `now:` line gives the condition's state, `waiting` while it reads false, `unknown` while an unread key leaves it undecided, or `due` when it already holds, then each key's reading, or for an unread `pr` or `ci` key the reason it is unread. With the room closed, `pr` and `ci` read as unread:

```text
  now: unknown · pr: readings paused (room closed)
```

A watch whose expression and checkout match a pending watch of your own session arms nothing: it prints `already waiting <name>: when <expression> → <handle>`, then your list.

`--json` prints `{"name", "trigger", "target", "pending"}`, adding `"existing": true` when it named an existing watch. `trigger` is the receipt's trigger text. `pending` is the array [`rimz watch list --json`](#list-your-watches) prints.

### The expression

The expression takes the [condition grammar of `rimz loop add --when`](./loop.md#conditions) (`pr`, `ci`, `team.stage`, `window.*`, and the rest) without a `--for` hold. The scope is the checkout you arm from.

The scheduler records the condition's first reading and fires on its next tick when the condition still holds, so a `window.*` or `team.stage` condition true at arm time fires on the second tick. A tick is one pass of the scheduler that fires loop tasks and watches: the room's sidebar host while the room is open, or the [loop timer](./loop.md#timer) when no room is open. A `pr` or `ci` key also waits for a sidebar probe at or after that first reading, so a pull request merged before the arm fires only after the sidebar probes it again ([forge freshness](./loop.md#forge-freshness)).

A `window.*` term reads the room's current account for your provider kind, which can differ from the account your own session runs on. It is refused when that window cannot be read or its reading's reset has passed. To wake when a spent window has room again, watch `'window.5h.left>=50'` (or `window.7d.left`) with the room your work needs: it fires as soon as the window has that much room, which may be before the window resets. While the window is short of that room, the receipt's `now:` reading shows when it resets, when the provider reports one, in the configured timezone: `window.5h.left: 8 (resets 2026-11-17 13:52)`.

`pr` and `ci` are read by the room's sidebar. With the room closed they read as unread, and the watch stays armed until a sidebar reads them true. While the room is open, the reason names why a key stays unread: `not checked yet`, `no supported forge for this checkout`, `the checkout's HEAD is detached, so it has no branch to read`, a failing lookup, or no pull request yet. `rimz watch list` shows the same reasons on a waiting row, and `rimz loop show <watch-name>` prints a reason for every missing reading.

When the room's last reading already says your checkout has no supported forge (the scope is not in a git checkout or its repository is gone, the checkout has no `origin` remote or one RimZ cannot parse, or the `origin` host is neither `github.com` nor a host whose name contains `gitea`, `forgejo`, or `codeberg`; a supported host whose CLI, `gh` or `tea`, is missing or failing reads as a failing lookup instead), `rimz watch` refuses an expression with any `pr` or `ci` term, a term joined by `||` included, since that term can never be read. A checkout the sidebar has not read yet, a detached HEAD, and a closed room arm as before. A watch whose checkout turns out to have no supported forge after it is armed stays armed and needs you in [`rimz loop list`](./loop.md#loop-list).

Bare `pr` reads the pull request of your checkout's branch. In a RimZ worktree it ignores a merged or closed pull request opened more than five minutes before the worktree was created, so a branch name reused from older work does not read as `merged`. The pull request the worktree was created from, and one whose creation time the forge does not give, always count. `pr[<branch>]` has no such guard: it accepts whatever pull request the forge returns for that branch name.

### Check-ins: `--check-in`

A check-in is one message sent while the expression still reads false. It cancels nothing: RimZ sends it once per watch, on the first tick at or after the watch's arm time plus the check-in that reads the expression false, and the watch stays armed. It reads `still not met after <ELAPSED>`. A paused or disabled watch checks in once it is live again, and a watch canceled or fired before its check-in is due sends none.

An explicit `--check-in` always applies. Without one, RimZ decides at arm time, from what it knows of the calling agent:

- When [cache keepalive](../../guide/configuration.md#idle-compaction) pings that agent, the watch gets no check-in. Keepalive pings take its place.
- Otherwise the check-in is `30m`, independent of `loop.default-timeout`.

A configuration change does not alter check-ins already recorded.

### Refusals

Each of these prints `error: ` and the message, exits 1, and arms nothing. An invalid `--label` value, or a duration that does not parse, is an argument error instead (exit 2).

| Error | Cause |
| --- | --- |
| `a watch needs an expression` | Options without an expression, or an empty or whitespace-only one. |
| `when: clause <N>, position <M>: <fix>` | An expression that does not parse, or names a team stage no team defines. |
| ``when: `<key>` cannot be read in <checkout>: no supported forge for this checkout; RimZ reads a git checkout whose `origin` remote is on github.com (through `gh`) or on a host whose name contains gitea, forgejo or codeberg (through `tea`), so wait on something else here`` | A `pr` or `ci` term, when the open room's last reading of your checkout found no supported forge ([the expression](#the-expression)). |
| `<kind> chooses its account per launch, so the room has no <kind> account whose windows a wait can read; window.* terms do not support <kind>` | A `window.*` term, from a provider kind that RimZ launches on an exact managed account. |
| ``cannot resolve the room's <kind> account; run `rimz accounts list` `` | A `window.*` term, when the room has no account for your provider kind. |
| ``no current <kind> <span> window reading; open the room's sidebar or run `rimz accounts --refresh` `` | A `window.*` term, when the room's account has no reading, or its reading of that window has a reset that has already passed. |
| `<kind> has no <span> window` | A `window.*` term, when the account's reading has no window of that span. |
| `--check-in must be greater than zero` | Zero for `--check-in`. |
| `--check-in must be less than 24h` | `24h` or more for `--check-in`. |
| `arming a watch is only available to an agent RimZ can identify; run this command from an agent pane` | Run from a user shell, or from a process RimZ cannot trace to an agent. |
| `nothing was armed: RimZ could not find the calling agent's live session from this process` | The caller's identity is set but no live session is visible, for example when a command sandbox hides the agent's process. The error names the kind and the launch id, or the pane for a legacy caller, and gives the [sandbox remedy](./shell.md#inside-a-providers-command-sandbox). |
| `RimZ identified the calling agent but its live session is unavailable` | The calling agent's session has ended. |
| `the calling agent has not registered a real session yet` | The agent has not reported its provider session yet. |

## List your watches

`rimz watch list` (alias `ls`, or bare `rimz watch`) shows your own watches from an agent pane. Flags: `--json`. It lists no shells; [`rimz shell list`](./shell.md#list-your-shells) does.

```console
$ rimz watch list
@coder#probe (@smart-beacon) · 1 watch
  watch-patient-maple  waiting  when team.stage=Review
```

The header is `<handle> · N watches` (`1 watch`), or `<handle> · no watches`. Each watch is one line with its name, state, and `when <expression>`, followed by ` · <label>` when one is set, never clipped to the terminal width. A worktree suffix (`· in <worktree>`) appears only when the row belongs to a checkout other than the one the command runs in. A row an older RimZ armed for a delay lists as `pending · fires in 28m` (`once at <YYYY-MM-DD HH:MM>`), and one for a clock time as `waiting`, and each fires as armed.

| Row | Displayed state |
| --- | --- |
| Condition | `waiting`. A condition with an unread `pr` or `ci` key adds each key's reason: `waiting · pr: no supported forge for this checkout`. |
| Paused | `paused · resumes in 2h`, after [`rimz loop pause`](./loop.md#enable-disable-and-pause) on the watch's name. |
| Disabled | `disabled`, after [`rimz loop disable`](./loop.md#enable-disable-and-pause) on the watch's name. |

Loop and team subscriptions that target your session are not watch lines. They appear on one line, `also woken by <triggers>[ in <worktree>] (loop <name>)` or `(team <name>)`, one group per owner, joined by ` · `. That line follows your watches, or joins the header when you have none. A delivery written by hand in configuration shows as a loop subscription under its own name.

From a user shell, the view is read-only:

```console
$ rimz watch
~/room · whole room · 1 watch

  @coder#probe (@smart-beacon)
    watch-patient-maple  waiting  when team.stage=Review
```

The header is `<home-relative project root> · whole room · N watches · M subscriptions` (`1 watch`, `1 subscription`), omitting a zero count, or `no watches` with no body. The body is the same room body as [`loop list --all`'s WAKE-UPS](./loop.md#loop-list), without shells: self-wait cards, with children nested under their ancestors and each handle paired with a differing name, followed by team and loop owner summaries. Cards use the same current addressable handles as receipts. A target that is no longer addressable, including a session shadowed by the pane's current owner, falls back to the recorded target handle. Hand-written configuration deliveries appear in the room view too.

`--json` prints every delivery but a shell that targets the caller's session, loop and team subscriptions included, or from a user shell the room view's rows. Each row's keys, in order:

| Key | Value |
| --- | --- |
| `name` | The watch or subscription name. |
| `type` | `condition` (a watch), `signal` (a subscription), or, for a row an older RimZ armed, `timer` (a delay) or `schedule` (a clock time). |
| `state` | `pending`, `waiting`, `disabled`, or `paused`. |
| `target` | The recorded target handle. |
| `trigger` | The trigger text, with a paused row's ` · resumes <when>` suffix. |
| `label` | Present only when set. |
| `dir` | Present only when recorded: the home-relative checkout path. |
| `elapsed`, `elapsed_s` | `null` for a watch. For an older delay row, the age since arming, as a label and in seconds. |
| `check_in`, `check_in_s` | `null` for a watch. For an older delay row, the requested delay. |
| `owner` | `self`, `loop:<task>`, or `team:<team instance>`. |
| `parent` | The target's launched parent's full handle over the current addressable roster, or `null` for a root agent, or a target or parent that is not addressable. |
| `unread` | A waiting condition's unread `pr` and `ci` keys, each mapped to its reason. Present only when some key is unread. |

The arm and cancel receipts' `pending` arrays are this same list.

`list` refuses the way [`rimz shell list`](./shell.md#list-your-shells) does when the caller's live session cannot be found or the RimZ home is read-only, and gives the same [sandbox remedy](./shell.md#inside-a-providers-command-sandbox).

## Cancel a watch

`rimz watch cancel <name>` cancels one pending watch, or one subscription, that targets the caller. `rimz watch cancel --all` cancels only the watches you armed, and leaves loop and team subscriptions in place. Give a name or `--all`: both, or neither, is an argument error (exit 2). Flags: `--all`, `--json`.

```console
$ rimz watch cancel --all
canceled watch-handy-silver, watch-patient-maple
@coder#probe (@smart-beacon) · no watches
```

Cancel removes each selected row. If RimZ cannot write the watch's home-wide state, cancel refuses before it removes the row. It prints `canceled <name>, ...` and then your list. `--all` that finds no watch you armed leaves out the `canceled` line and exits 0. `--json` prints `{"canceled": [...], "pending": [...]}`, where `pending` holds every remaining delivery but a shell, subscriptions included.

| Error (exit 1) | Cause |
| --- | --- |
| `canceling a watch requires an agent RimZ can identify; run this command from an agent pane` | Run from a user shell. |
| ``no pending watch named `<name>`; see `rimz watch list` `` | The name is not in the caller's watch list. A shell's name is refused here too: cancel it with [`rimz shell cancel`](./shell.md#cancel-a-shell). |
| `nothing was canceled: RimZ could not find the calling agent's live session from this process` | The caller's identity is set but no live session is visible. The error names the kind and the launch id, or the legacy pane, and gives the [sandbox remedy](./shell.md#inside-a-providers-command-sandbox). |
| `RimZ identified the calling agent but its live session is unavailable` | The calling agent's session has ended. |
| `nothing was canceled: this command cannot write the RimZ home` | The RimZ home is read-only. The original cause and the sandbox remedy follow. |

## The delivered message

A watch arrives as a message from `@rimz` with `Type: WAIT` ([the message header](./message.md#the-message-header)), sent as a steer: it is typed into the live turn at once. When the agent has its own question or permission prompt open, the wake parks instead and lands at the next turn boundary after the prompt is answered. A check-in is delivered the same way, and when the watch fires, RimZ cancels a check-in still parked. [`rimz transcript`](./transcript.md) hides wait messages from its rendered view and keeps them in `--json`.

The body names the expression and any label, then `fired after <ELAPSED>` with the watch's name in brackets, then each key's reading on its own line:

```text
waited on team.stage=Review · review it
fired after 1s [watch-mint-spring]
team.stage: Review
```

A check-in reads `still not met after <ELAPSED>`, puts `· still watching` right after it, and ends with the command that cancels the watch:

```text
waited on team.stage=Done · ship it
still not met after 1m · still watching [watch-handy-silver]

Stop it: rimz watch cancel watch-handy-silver
```

A row an older RimZ armed for a delay delivers one line, `waited <DURATION> [<name>]`, and one for a clock time `waited until <YYYY-MM-DD HH:MM> [<name>]`.

## While a watch is pending

An agent at rest with a pending watch, or a one-shot signal delivery, shows `sleeping` (`☾` in the sidebar) where it would show `idle` or `success`. A running [shell](./shell.md#while-a-shell-runs), unfinished team, or working child shows `running` instead. Child waits follow the target's work through further waits. A settled child's report contributes neither state. A standing (repeating) signal subscription does not count. The [status table](./agents.md#list) gives the precedence.

In a supervised run, arming a watch and ending the turn does not finish the run: it stays `running` and keeps its pane until the work ends, and the wake starts the next turn in that same run. If the wake is lost and nothing else remains to wake the parked run, the run fails with exit `1` and a reason beginning `parked on a wake that never arrived`. A run held only for a person's or an agent's message that is then withdrawn completes instead. The run's timeout still applies ([supervised runs](./agents.md#supervised-runs--p)).

A sleeping agent still takes messages: a normal message starts a turn and leaves the watch armed. A team member with a pending watch does not emit `team.idle`. The sidebar card counts pending shells and watches as `⧖ waits (N)` and lists each one when expanded ([the card](../../interface/sidebar.md#the-card)).

## What a watch writes on your machine

A watch is one row in `~/.rimz/ws/<workspace-dir>/records/loop-instances.json`, with no watcher process. It never touches `loop.toml` or project config. Its `when` stores the expression, and its optional `label` the trimmed description. Whether its check-in was sent is kept in the runtime cache `loop-check-ins.json`, so a lost cache can send that one check-in again.

The row retires when the expression fires, the watch is canceled, or the agent ends, is lost, or is stopped. A restart or molt of the agent keeps the row, and the wake reaches the session that holds the agent when it fires. A check-in does not retire it. `rimz gc` removes rows left behind.

Every fire writes loop history, which `rimz loop logs <name>` keeps readable after the watch retires. Once its run ends, `rimz loop show <name>` refuses and points to those logs, with `--root <root>` when the retained runs are in another known room. `logs` keeps its own room selection, so pass that root there too. The mechanics are in [waits](../../internals/harness/loops.md#waits).

## Exit codes

| Code | When |
| --- | --- |
| `0` | A watch was armed or an existing watch named, a list printed, or a cancel finished, including `cancel --all` with nothing to cancel. |
| `1` | A refusal: an [arm refusal](#refusals), a [list refusal](#list-your-watches), or a [cancel refusal](#cancel-a-watch). Nothing was armed or canceled. |
| `2` | An argument error: an invalid value, a duration that does not parse (a compound one included), or `cancel` with both a name and `--all`, or neither. |

For CI and pull request state, watch an expression or fire a [loop task on a signal](./loop.md#signals) instead of polling forge commands in a shell.

# Notifications

You already wire your own alerts: `claude -p "..." ; notify-send done` at the end of a command, a bell on the last line of a script, a `curl` to a Slack webhook when the build goes red. All of them fire when a process exits, and exiting is the one thing an agent that needs you does not do. Ten minutes into an hour of work it hits a permission prompt and sits there, still running, still holding its context, with nothing to tell the shell that started it.

RimZ knows the moment it happens: the card flips to `? waiting` and rises in the sidebar ([sidebar](./sidebar.md)). Notifications push that same moment to wherever you actually are. A desktop banner and a bell go out on their own, and a handler runs any command you like, which makes the last mile a route you already own: ntfy or Pushover on your phone, a Slack webhook, `notify-send`, or a script that answers the routine prompt so you never see it.

## What fires before you configure anything

A card goes unread whenever its agent stops for you: `waiting`, `failed`, `paused`, or `success`. Two of those also push a notification, `waiting` and `failed`. The other two stay quiet until you [widen the triggers](#tune-what-reaches-you).

A push writes a desktop notification into your terminal, which turns it into a native OS banner. The title is `RimZ: <label> needs you`, over a line naming what the agent wants. The label is the agent's task or prompt where it has one, trimmed to 48 characters, so the banner names the work that is blocked rather than the handle behind it.

The same push writes a bell, which rings if your terminal makes a sound and marks the tab the pane sits in. Both escapes cross tmux and SSH. RimZ turns tmux's `allow-passthrough` on in its own rooms, so they reach your terminal from a window you are not looking at, and nothing has to be installed on the machine you are sitting at beyond the terminal already drawing the room.

Three rules keep that from becoming noise, and the same three explain a flip that stayed silent. A card whose pane you are already watching never pushes. Agents that flip within the same second are batched into one banner, `RimZ: 2 agents need attention`, whose body names each. And one agent cannot push twice inside five seconds. Attaching to a room that ran without you is quiet in the same spirit: the first pass renders everything that happened while you were away as unread cards, with no burst of banners behind them.

What you leave unread keeps nudging. Every 60 seconds, for as long as an unread card is `waiting` or `failed`, the sidebar rings again with `RimZ: 2 unread rows need you`. Unread `paused` and `success` cards stay emphasized in the column and never nudge. The nudging stops when you look at the card, which is also when the tab marker and the card's unread state clear.

A banner your terminal never draws costs you nothing: the card stays unread and ranked in the sidebar until you look at it.

One gap to know before you lean on a banner: Zellij drops notification escapes, so RimZ does not write them there. The bell and the tab marker still work, `desktop = "osc"` writes the escape anyway if your setup passes it through, and a handler reaches the OS on either multiplexer. When no banner appears anywhere, [troubleshooting](./troubleshooting.md#notifications-dont-fire) walks the chain from terminal support to OS permission.

## Push it anywhere with a handler

A banner reaches the terminal you walked away from. A handler reaches you. It is one table in your per-machine `~/.rimz/config.toml`, naming a command to run when a notification matches:

```toml
[[notifications.handler]]
name = "phone"
command = "ntfy publish --title {{title}} rimz {{body}}"
when = { kind = ["waiting", "failed"] }
```

That is a whole handler. `name` is yours to recognize it by, `command` is anything a shell can run, and `when` decides which notifications reach it. Add as many tables as you have routes.

Handlers live only in that per-machine file. A project's `.rimz/config.toml` cannot define one and they sit outside project trust, so cloning a repository never brings a command with it. That is deliberate: this is personal routing, and it usually carries push credentials for one machine ([security](./security.md)).

### What RimZ runs

Every matching handler is spawned once per notification, in the order the tables appear, as `sh -c "<your command>"`. Three consequences are worth planning for.

It is not your login shell. No `.zshrc`, no aliases, no functions: give your own scripts absolute paths. What the command does inherit is the environment of the process that noticed, which for an agent notification is the room's sidebar, so a `rimz` command inside a handler addresses the room that fired it whatever directory it runs from.

Nobody reads its output. stdin, stdout, and stderr go to `/dev/null` and RimZ never waits for the exit code, so a handler that fails fails silently. Write anything you want to read later to a file of your own.

It runs on the machine the notification came from. Agent notifications and the 60-second nudges fire from the room's sidebar, so handlers for a room on a server run on that server, with that server's credentials. `link_lost` and `link_restored` fire from `rimz remote connect` on the machine you typed it into, and `loop_disabled` from `rimz loop` wherever the task runs.

A fourth trap is in the file rather than the spawn, and it costs more than the handler carrying it. An unknown `{{name}}`, an invalid glob, or an empty command makes the whole `[notifications]` table invalid, and the sidebar falls back to the built-in defaults, which have no handlers at all. Check a new template against the table below before you rely on it.

### Narrow it with when

An empty `when` matches every notification. Add one clause and it has to match; add several and all of them do. Each takes a list, and one value matching is enough for its clause.

`kind` names notification kinds: the four agent statuses (`waiting`, `failed`, `paused`, `success`), `coalesced` for a batch of them, `reminder` for the 60-second nudge, `loop_disabled` when a scheduled task disables itself after three failed fires ([loops](./loops.md#budgets-and-strikes)), and `link_lost` or `link_restored` when a supervised SSH link drops or recovers ([remote](./remote.md#a-link-that-heals-itself)).

`worktree` glob-matches the agent's branch, or its worktree path when it has no branch. `handle` glob-matches its handle or role, with or without the leading `@`. So `when = { kind = ["waiting"], handle = ["@planner"], worktree = ["release/*"] }` sends one agent's questions on a release branch to your phone, while the noisy experiment beside it stays quiet.

One trap: a `reminder`, a `loop_disabled`, and both link kinds name no agent, so a handler carrying a `worktree` or `handle` clause never sees them. On a `coalesced` batch, each clause may be satisfied by a different agent.

### What the handler receives

The event arrives twice over: as `{{name}}` substitutions in the command, and as environment variables for a script that would rather read the environment.

| Template | Environment | Value |
| --- | --- | --- |
| `{{title}}`, `{{body}}` | `RIMZ_NOTIFY_TITLE`, `RIMZ_NOTIFY_BODY` | The banner text RimZ rendered |
| `{{kind}}` | `RIMZ_NOTIFY_KIND` | The notification kind |
| `{{agent}}`, `{{handle}}` | | The agents' handles or roles, joined with `, ` |
| | `RIMZ_NOTIFY_AGENT` | The agents' card labels, which prefer the task or prompt over the handle, joined with `, ` |
| `{{count}}` | | How many agents the notification names |
| `{{status}}`, `{{task}}`, `{{worktree}}` | | The single agent's status, task, and branch or worktree path |
| `{{pane}}`, `{{root}}` | `RIMZ_NOTIFY_PANE`, `RIMZ_NOTIFY_ROOT` | The single agent's pane id and worktree path |
| | `RIMZ_NOTIFY_ASK` | The id of the open prompt, on a one-agent `waiting` notification |
| `{{unread}}` | `RIMZ_NOTIFY_UNREAD` | How many cards are unread, on a nudge |

A value the notification does not carry is the empty string, and every variable but `RIMZ_NOTIFY_UNREAD` is always set, so a script tests for a value rather than for the variable. The four rows from `{{status}}` down describe a single agent, and all of them are empty when the notification names several agents or none. In the command, each substituted value is shell-quoted as one token, so write `--title {{title}}` and not `--title "{{title}}"`.

### Test it before you need it

When the handler is a script, run it the way RimZ will, with the environment filled in by hand:

```sh
RIMZ_NOTIFY_KIND=waiting \
  RIMZ_NOTIFY_AGENT='refactor the parser' \
  RIMZ_NOTIFY_TITLE='RimZ: refactor the parser needs you' \
  RIMZ_NOTIFY_BODY='refactor the parser is waiting for input.' \
  sh -c "$HOME/.rimz/approve-reads.sh"
```

That proves your script and your push route. It does not prove the `when` clause or the templates, which is what the second check covers. From inside the room, `rimz sidebar notify-test @coder` builds a real notification for a live card and sends it down the real path, handlers included; `--kind <kind>` picks the kind to simulate (default `waiting`) and `--no-command` skips the handlers to test only the banner and the bell. It is a diagnostic rather than a daily verb, so `rimz sidebar --help` leaves it out.

### Claude and Codex push on their own

If your fleet is Claude Code or Codex, there is a route with no handler to write. Both ship a bridge to the provider's official mobile app, and RimZ keeps it running with the room on a per-machine toggle: the ask pushes to your phone and you answer the native prompt there ([remote → answer asks from your phone](./remote.md#answer-asks-from-your-phone)). Handlers stay the portable route, since they reach any agent, any channel, and any script. Whichever one brings the news, you answer the same way: reattach over SSH ([remote](./remote.md)), open the room in a browser ([web](./web.md)), or wait until morning, because the card is still there.

## Handlers that answer

By the eighth time you approve the same file read, the notification has stopped carrying information. A handler fires with the id of the prompt in hand, and every move after that is a public RimZ command, so a handler can take the decision you would have taken and still leave everything else for you.

This pair approves Claude's read-only tool calls for the documentation agents and stays out of the way otherwise:

```toml
[[notifications.handler]]
name = "approve-reads"
command = "$HOME/.rimz/approve-reads.sh"
when = { kind = ["waiting"], handle = ["@docs*"] }
```

```sh
#!/bin/sh
# ~/.rimz/approve-reads.sh
[ -n "$RIMZ_NOTIFY_ASK" ] || exit 0
ask=$(rimz asks show "$RIMZ_NOTIFY_ASK" --json) || exit 0
[ "$(printf '%s' "$ask" | jq -r .kind)" = permission ] || exit 0
case "$(printf '%s' "$ask" | jq -r '.detail // ""')" in
  Read:* | Glob:* | Grep:*) rimz answer "$RIMZ_NOTIFY_ASK" allow ;;
esac
```

`rimz asks show <id> --json` prints the prompt as structure: its `kind` (`permission`, `plan_approval`, or `question`), the tool call in `detail`, and the options an answer may pick, each carrying a `caution` when choosing it changes more than this one prompt ([ask JSON](../reference/cli/asks.md#ask-json)).

`rimz answer <id> <choice>` validates the choice against that list, types it into the agent's own UI, and waits for the agent to move, so the prompt and the answer land in the transcript exactly as if you had typed them. Claude permission prompts accept `allow`; plan approvals accept `approve`, which also turns on auto-accept edits, so a script should not reach for it blind. Which prompts each agent accepts is [a table in the asks reference](../reference/cli/asks.md#what-each-agent-accepts).

The same handler can reach for anything else the room exposes. `rimz pane capture "$RIMZ_NOTIFY_PANE"` and `rimz pane send "$RIMZ_NOTIFY_PANE" 2 --enter` drive a prompt shape `rimz answer` does not support. `rimz message @reviewer "look at the migration @coder is stuck on"` hands the situation to another agent. `rimz agents codex -p "Is this diff safe to approve? answer yes or no"` spends a whole run on the decision ([scripting](./scripting.md)).

Three rules keep an acting handler honest.

Test the id, not the variable. `RIMZ_NOTIFY_ASK` reaches every handler and is empty unless the notification names exactly one `waiting` agent with a prompt open. Two agents flipping together arrive as one `coalesced` notification with no id, and a nudge carries none either; both still reach you through the sidebar's unread cards.

Treat the prompt as untrusted. `detail` and the question text are agent output and can say anything at all. Match shapes you chose in advance, and do nothing on everything else ([security](./security.md)).

Leave what you did not decide. Whatever the handler ignores stays `? waiting`, ranked, and routed to you exactly as before, which is what makes a pattern safe to add one shape at a time.

## Tune what reaches you

Everything above is what RimZ does untouched, and `[notifications]` in `~/.rimz/config.toml` changes it.

Four keys quiet it. `sound = "off"` stops the bell and the tab marker, `desktop = "off"` stops the banner, `remind_secs = 0` stops the 60-second nudge, and `enabled = false` stops all of that and the handlers with it, leaving the unread cards in the sidebar untouched. One thing outlives it: a scheduled task that disables itself still fires `loop_disabled` handlers.

Two widen it. `triggers = ["waiting", "failed", "success", "paused"]` adds the two statuses that stay quiet by default, so a finished run pings you too, and `suppress_focused = false` pushes even for the pane you are watching.

`title` and `body` reword the banner. They take the same `{{name}}` substitutions as a handler command, minus `{{title}}` and `{{body}}` themselves, and replace the built-in text for agent and coalesced notifications. Nudges and link alerts keep theirs.

Two keys set the pace. `debounce_ms` (5000) is the floor between two pushes for one agent, and `coalesce_ms` (1000) is the window that batches several agents into one notification; at `0` a flip pushes as soon as the sidebar sees it, without waiting for a companion.

Every `[notifications]` key with its default is in [configuration](./configuration.md#notifications).

## See also

- [Sidebar](./sidebar.md): the unread inbox and the ranking these notifications mirror.
- [Web](./web.md): open the room in a browser, which is how you answer when the push lands on a phone.
- [Configuration](./configuration.md#notifications): every `[notifications]` key and its default.
- [Security and trust](./security.md): handlers as one of the two places config can run a command, and what that means for a clone.
- [Remote → answer asks from your phone](./remote.md#answer-asks-from-your-phone): the provider's own mobile push and answer, with no handler to write.
- [Loops and schedules](./loops.md): the other half of unattended work, and where `loop_disabled` comes from.
- [Scripting](./scripting.md): the runs a handler can start, and the exit codes they hand back.
- [Troubleshooting](./troubleshooting.md#notifications-dont-fire): when no desktop banner appears anywhere.
- [Asks and answers](../reference/cli/asks.md): the ask JSON, the answer selectors, and what each agent's prompts accept.
- [Notifications internals](../internals/sidebar/notifications.md): the producer and renderer split, the reminder loop, and the trace log behind a stray tab marker.

# Notifications

> When an agent needs you and you are not looking, RimZ taps you: a desktop banner, a terminal bell, and — through notification handlers — any channel or script you own. This page wires the tap to your phone, your chat, or a script that clears routine prompts itself. The trigger model, debounce, and delivery plumbing are [internals → notifications](../internals/sidebar/notifications.md).

## Why notifications

RimZ's job is routing your attention, and the sidebar does it while you watch: a row flips to `? waiting`, rises, and one keystroke lands you in the right pane ([sidebar](./sidebar.md)). The moment that costs you is when you are not watching. You are heads-down in another pane, in a meeting, detached over SSH, asleep — and an agent hits a permission prompt ten minutes into an hour of work. The blocker is a one-line answer; unseen, it stalls the agent for exactly as long as you are away.

You also already own the last mile. ntfy or Pushover on your phone, a Slack webhook, `notify-send` on your desktop — every developer has a push route that reaches them. RimZ ships no push service of its own: a handler runs your command with the event in hand, and your route does what it already does.

## What you get without configuring anything

An agent going `waiting` or `failed` marks its card unread in the sidebar, writes a terminal notification escape that your terminal turns into a native desktop banner — through tmux and SSH included, since RimZ rooms enable passthrough by default — and rings the bell. A row that stays waiting earns one reminder nudge rather than a stream, and several agents flipping at once coalesce into one notification. All of it is best-effort polish over the durable store: a missed banner loses nothing, because the card stays unread and ranked until you look.

Two caveats are worth knowing up front. Zellij currently drops desktop notification escapes, so on Zellij the handler path below is the route to a native banner. And when no banner appears anywhere, [troubleshooting](./troubleshooting.md#notifications-dont-fire) walks the checklist from terminal support to OS permission.

## Handlers: your command on the room's cue

A handler is one table in `~/.config/rimz/config.toml`. When a matching cue fires, the room's sidebar process runs your command:

```toml
[[notifications.handler]]
name = "phone"
command = "ntfy publish --title {{title}} rimz {{body}}"
when = { kind = ["waiting", "failed"] }
```

That is the whole feature: `waiting` and `failed` are the default triggers (add `paused` or `success` via `notifications.triggers`), and the command is anything — a push CLI, a webhook `curl`, a script. The event arrives twice over: as template variables (`{{title}}`, `{{body}}`, `{{agent}}`, `{{kind}}`, `{{worktree}}`, `{{pane}}`, `{{root}}`), each substituted as one shell-quoted token so you write them bare, and as environment variables (`RIMZ_NOTIFY_AGENT`, `RIMZ_NOTIFY_KIND`, `RIMZ_NOTIFY_PANE`, `RIMZ_NOTIFY_ROOT`, plus `RIMZ_NOTIFY_ASK` on a single-agent waiting notification and `RIMZ_NOTIFY_UNREAD` on reminders) for scripts that prefer reading the environment.

A `when` clause narrows a handler; its present conditions all have to match, and an empty `when` matches everything. `kind` names the notification kinds — the agent statuses plus `coalesced`, `reminder`, and the `link_lost`/`link_restored` pair that [`rimz remote connect`](./remote.md) fires when an SSH link drops or recovers. `worktree` glob-matches the agent's branch, and `handle` glob-matches its handle or role, so the noisy experiment stays quiet while `@planner` on `release/*` reaches your phone.

Handlers live in per-machine config and stay outside project trust, because they are personal routing that often carries host-specific push credentials — a cloned repository never brings its own ([security](./security.md)). The payoff of the phone route: a failing migration at 3 a.m. becomes a push notification, you open the room from wherever you are ([remote](./remote.md), [web](./web.md)), answer in the agent's own UI, and the pipeline is green by morning.

For Claude and Codex, [remote control](./agents.md#answer-asks-from-your-phone) closes the same loop through the provider's own app: the room starts the official mobile bridge on your opt-in, the ask pushes to your phone, and you answer the native prompt right there, with no handler to write and no reattach.

## Handlers that act, not just alert

A handler fires with the pane and root in hand, and everything it might do next is a public RimZ command. That makes a handler a place to clear the routine prompt you have already approved eight times today, composed from the room's own primitives:

- `rimz asks show "$RIMZ_NOTIFY_ASK" --json` reads the structured prompt and safe choices.
- `rimz answer "$RIMZ_NOTIFY_ASK" <choice>` validates and types one atomic supported answer into the agent's own UI. Claude permission notifications offer `allow`, and plan approvals offer caution-marked `approve`; the remaining menu actions route the human to the pane.
- `rimz pane capture @<handle>` and `rimz pane send @<handle>` remain the escape hatch for an unsupported prompt shape.
- `rimz message @<other>` hands the situation to a different agent.
- `rimz agents <kind> -p` runs a one-shot [supervised turn](./scripting.md) to decide.

A handler can match the prompt against patterns your script owns and answer only the shapes it recognizes — a bounded-pattern approver, a one-shot agent delegate, or a standing in-room guardian you steer with `rimz message --steer @guardian`. Anything the handler leaves alone stays `? waiting` in the sidebar and still routes to you. Attention bandwidth then scales with what you automate rather than with the agent count. Because pane text is agent output and can contain anything, treat it as untrusted: match known shapes, do nothing on the unknown ([security](./security.md)).

## See also

- [Sidebar](./sidebar.md) — the unread inbox and attention ranking these notifications mirror.
- [Agents → answer asks from your phone](./agents.md#answer-asks-from-your-phone): the provider's own mobile push-and-answer bridge, started with the room.
- [Loops and schedules](./loops.md) — the other half of unattended work: schedules, watchdogs, and the permission posture.
- [Remote](./remote.md) and [web](./web.md) — answering from another machine or a phone once the push lands.
- [Troubleshooting](./troubleshooting.md#notifications-dont-fire) — when no desktop banner appears.
- [Configuration](./configuration.md#notifications) — every `[notifications]` key: triggers, debounce, coalescing, reminders, templates.
- [internals → notifications](../internals/sidebar/notifications.md) — the producer/renderer split, the unread model, and the trace log.

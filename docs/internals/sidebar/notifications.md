# Notifications

Notifications push the sidebar's attention signal to a user who is not looking at it: a desktop banner, a terminal bell, a reminder, or a command the user configured. They are best-effort. The durable inputs are the store and the unread episode set, so a missed notification costs latency and the row stays unread in the sidebar until someone reads it.

This page owns the push path: who decides a notification, who delivers it on each channel, the reminder loop, handler matching and templates, and the trace log. Unread episodes and the read receipts that clear them are [sidebar.md → Unread and read receipts](./sidebar.md#unread-and-read-receipts). The user-facing pages are [the notifications guide](../../guide/notifications.md) and [configuration → Notifications](../../guide/configuration.md#notifications).

## Where it lives

| Path | Role |
| --- | --- |
| [`sidebar/notify.rs`](../../../crates/rimz/src/sidebar/notify.rs) | `NotificationState` push policy over newly opened episodes, title and body rendering, `spawn_notify_handlers`, and `LinkNotificationState` for the link-health record |
| [`sidebar/unread.rs`](../../../crates/rimz/src/sidebar/unread.rs) | `UnreadEpisodes`: `unread.json`, `reconcile`, and the `OpenedUnread` records the policy consumes |
| [`sidebar_pane/app/fetch.rs`](../../../crates/rimz/src/sidebar_pane/app/fetch.rs) | `evaluate_notifications` and `deliver_notifications`, run by the host's fetch worker on each final fold |
| [`sidebar_pane/app/notify.rs`](../../../crates/rimz/src/sidebar_pane/app/notify.rs) | `emit_terminal_notification`: the renderer's desktop targeting and `bell_decision` |
| [`sidebar_pane/app/remind.rs`](../../../crates/rimz/src/sidebar_pane/app/remind.rs) | `RemindState`: the fetch worker's room-wide unread reminder timer |
| [`osc.rs`](../../../crates/rimz/src/osc.rs) | OSC 777 and BEL bytes, the tmux DCS wrap, and the terminal-local variant for processes outside a sidebar pane |
| [`config/notifications.rs`](../../../crates/rimz/src/config/notifications.rs) | `NotificationsPrefs`, `NotificationKind`, handler conditions, template rendering and validation |
| [`diag/notify.rs`](../../../crates/rimz/src/diag/notify.rs) | The `audit/notify.log.jsonl` record schema |

## Channels

| Channel | Mechanism | Crosses SSH | tmux | Zellij |
| --- | --- | --- | --- | --- |
| In-band attention | Sidebar rows, ranking, unread emphasis | n/a | yes | yes |
| Desktop banner | OSC 777 written by a renderer, DCS-wrapped under tmux | yes | yes | off under `desktop = "auto"` (`mux::drops_desktop_osc`) |
| Sound and tab marker | BEL written by a renderer | yes | yes | tab `[!]` marker, audibility is the terminal's |
| Handlers | `sh -c` commands from `[[notifications.handler]]` | command-defined | yes | yes |

The sidebar is the authoritative surface, and every other channel mirrors it. The terminal turns OSC 777 into an OS banner when it supports the escape. Handlers are the portable route: they reach a push service, a detached room, or a Zellij user whose terminal never sees the OSC.

## Who emits which kind

Five code paths construct a notification, and each sets its `NotificationKind`. Handlers match on that kind, and `RIMZ_NOTIFY_KIND` carries it.

| Kind | Emitter | Channels |
| --- | --- | --- |
| `waiting`, `failed`, `paused`, `success`, `coalesced` | The host, over newly opened unread episodes ([the host](#the-host)) | Handlers, then `SidebarEvent::Notify` to every renderer for OSC and bell |
| `reminder` | The host, over the room's unread scope ([unread reminders](#unread-reminders)) | Handlers once, then `SidebarEvent::Notify` to every renderer |
| `link_lost`, `link_restored` | The local `rimz remote connect` supervisor ([remote link alerts](#remote-link-alerts)) | OSC and BEL on the supervisor's stderr, and handlers |
| `loop_disabled` | `rimz loop` when a task auto-disables after consecutive failed fires (`crates/rimz/src/cli/loop_cmd/run.rs::notify_loop_disabled`) | Handlers, then a broadcast `Notify` event with no panes |
| any but the two link kinds | `rimz sidebar notify-test <target>`, a hidden verb in `cli/sidebar.rs` | Handlers unless `--no-command`, then a broadcast `Notify` event for the target rows' panes |

Only the agent episode path applies triggers, debounce, and coalescing. Agent notifications and reminders both honour focus suppression, and the host writes a `notification_emitted` trace record for each one it delivers. The CLI emitters write no such record.

`notify-test` takes `--kind` (default `waiting`), `--title`, `--body`, `--worktree`, `--force-bell`, and `--no-command`. It refuses when `enabled = false`, so a test never reports success for a channel that is switched off.

## The host

The room host's fetch worker decides agent notifications once for all its panes ([state.md → The cycle](./state.md#the-cycle)). It evaluates on every final fold, before publication, and delivers after publication. Every cycle ends in a final fold, and in a quiet room the worker's idle backstop supplies the cycles, one per data tick (1 s by default, [state.md → Requests](./state.md#requests)). A coalesce flush or a reminder that comes due between folds therefore fires on the next one, at most one data tick plus that cycle's fold time late.

`evaluate_notifications` first calls `sidebar::unread::reconcile_snapshot`, which loads `unread.json` and the room read marks. A snapshot without a produced pane frame neither opens nor prunes, since a missing frame says nothing about which rows left the room. On a framed snapshot, reconcile does three things:

1. It prunes read marks whose rows left the room.
2. It prunes episodes a receipt reaches, silently, and episodes whose row has left the snapshot, with an `unread_cleared` record of cause `row_gone`.
3. It opens an episode for each row whose displayed status needs a look (`AgentStatus::needs_a_look`), that has no open episode, and that no receipt reaches at its `last_activity`.

The returned `OpenedUnread` list is the only input to push policy. An episode opened by `rimz sidebar mark-unread` or the renderer's `m` key goes straight to `unread.json` through `mark_rows_unread`, never appears in that list, and pushes nothing.

A missing `unread.json` makes every open in that pass `silent`. Attaching to a busy room therefore renders its current attention rows unread without a burst of banners. Once the file exists, the durable set dedupes across host replacement and renderer restart: an episode already recorded does not open again. A file that is unreadable, invalid, or of another version loads as empty but not absent, so its opens do push.

### Push policy

`NotificationState::evaluate` applies the user's policy to each opened episode, in this order:

1. With `enabled = false`, drop everything pending and push nothing. Episodes still open, so the sidebar still shows unread.
2. Skip a silent open, and a status outside `triggers` (default `waiting` and `failed`).
3. With `suppress_focused`, skip a row whose pane is in the snapshot's `viewed_panes`. That list comes from live mux focus, so this is a conservative visibility hint with no durable record behind it.
4. Skip an agent notified less than `debounce_ms` ago. Debounce is keyed by agent kind and session id, and a key is forgotten once its row leaves the snapshot.
5. Skip an agent already pending. Otherwise add it to the pending batch with its open ask id, if any.

The first addition to an empty batch starts the coalesce window, and the batch flushes once `coalesce_ms` has passed (at once when it is 0). A pending entry whose row is no longer unread is dropped before the flush, so a row read within the window pushes nothing. Flushing stamps every batched agent's debounce clock.

| Batch | Notification |
| --- | --- |
| One agent | The agent's status kind, with built-in text such as `RimZ: <label> needs you` |
| Several agents | One `coalesced` notification titled `RimZ: <N> agents need attention`, its body joining `<label>: <status>` pairs with ` \| ` |

`[notifications].title` and `.body` then replace the built-in text for the status kinds and `coalesced`. Reminders, link alerts, and `loop_disabled` keep their built-in text. A template that fails to render leaves the built-in text in place.

### Delivery

`deliver_notifications` handles each notification in this order:

1. It spawns the matching handlers (only when any are configured).
2. It appends `notification_emitted` to the trace log.
3. It sends `SidebarEvent::Notify` to each of the host's current subscribers, through a nonblocking `try_send` on that pane's in-process wake channel. The event carries the title, body, the agents' pane ids, `recheck_unread`, and the kind.

A full wake channel drops the notice for that pane. Publication precedes this fan-out, so the fold behind a notice is already in the host's views when a renderer handles it. The CLI emitters have no host, so they send their `Notify` through `wakeup::broadcast` to the room socket instead. Notification policy writes no terminal bytes: every attachment decides its own OSC and bell.

## The renderer

A renderer turns a `Notify` event into terminal bytes in `emit_terminal_notification`, outside the draw cycle. It decides the bell and the desktop banner separately, and with `enabled = false` it writes neither.

### The bell

The bell is bound to current unread attention. A BEL sets a tab or window marker the renderer cannot retract, so the marker must point at a row that still needs a look. `bell_decision` runs these checks in order, and the first that fails names the `suppressed` reason:

| Check | Reason when it fails |
| --- | --- |
| Notifications are enabled (checked before `bell_decision`) | `notifications_disabled` |
| The renderer has an own view (it sits in a tab with working panes) | `no_own_view` |
| The view is not a daemon-only view, whose siblings are infrastructure panes | `daemon_view` |
| A target pane is one of the view's working panes | `pane_not_in_view` |
| With `recheck_unread`, a target pane in the view maps to a row with `SidebarRow::unread` set | `not_unread` |

A bell that passes writes BEL when `sound = "bell"`. The unread bit stays set until a read receipt reaches the episode, so the row stays unread exactly as long as the marker has something to point at. A `Notify` event with no panes (`loop_disabled`) never rings. Agent notifications send `recheck_unread: true`. Reminders and `notify-test --force-bell` send `false`, a reminder because its scope is already unread.

### The desktop banner

The desktop banner is a reachability channel, so it is targeted per backend:

| Case | The renderer writes the banner when |
| --- | --- |
| tmux, any kind | It has an own view, whichever window the target sits in |
| Zellij, `reminder` | Its own tab is the watched tab, so one renderer writes one banner per reminder |
| Zellij, any other kind | Its view holds a target pane |

Under tmux, every renderer with an own view writes the banner, so it reaches the active client stream even when the agent sits in a background window. The mode then applies: `desktop = "auto"` skips the OSC under Zellij, `osc` writes it everywhere, and `off` never writes it. The text passes through `osc::osc_text`, which strips control bytes, turns newline, carriage return, and tab into spaces, and turns `;` into `:`.

tmux forwards the wrapped payload (`ESC P tmux; ... ESC \`, inner escapes doubled) only when `allow-passthrough` is on, and RimZ turns it on for its rooms by default (`[tmux] allow_passthrough`). Passthrough from an inactive pane and delivery to a detached session are up to the multiplexer and client, so handlers are the deterministic path for a user who is away from the terminal.

## Unread reminders

Reminders re-ring actionable rows the user has not read. The fetch worker owns one `RemindState` per room and evaluates it on each final fold, after unread reconciliation.

The reminder scope is the room's unread rows whose status is `waiting` or `failed` (`AgentStatus::is_actionable`) and that carry a pane, in any tab. With `suppress_focused`, a row whose pane is in `viewed_panes` leaves the scope. Unread `paused` and `success` rows stay emphasized in the sidebar but never remind.

The clock arms when the scope first becomes non-empty, and re-arms whenever the host delivers an agent notification. A reminder fires `remind_secs` (default 60) after the later of the arming and the previous reminder. An empty scope, `enabled = false`, or `remind_secs = 0` clears the clock.

A reminder spawns matching handlers once with kind `reminder`, title `RimZ: <N> unread rows need you` (`RimZ: 1 unread row needs you` for one), and `RIMZ_NOTIFY_UNREAD` set to the scope count. The host then fans out `SidebarEvent::Notify` targeting every scope pane, with `recheck_unread: false`. Each renderer applies the bell checks and the desktop targeting above.

## Remote link alerts

The local `rimz remote connect` supervisor alerts on link loss itself, because a dead link cannot carry the remote sidebar's output. It writes its terminal bytes to stderr, only when stderr is a terminal, through `osc::local_terminal_notification_bytes`. That function detects tmux or Zellij from the environment, honours `enabled`, `desktop`, and `sound`, and writes a plain OSC outside any multiplexer.

| Event | Kind | Terminal bytes | Handlers |
| --- | --- | --- | --- |
| Confirmed transport loss | `link_lost` | yes | yes |
| Recovery | `link_restored` | yes | yes |
| Probe blackout | `link_lost` | yes | no |
| Port forward parked on a busy local port | none | yes | no |

The supervisor loads `[notifications]` leniently at each alert. None of these events is sent as a `Notify` event, so no renderer sees them and the trace log has no record of them.

A link that is degraded but still passing bytes raises no bell, OSC, or handler. The footer badge shows it, and the remote host's `LinkNotificationState` writes a `link_alert` diagnostic at each episode edge. The edge rules, hold times, and the supervisor's outage actions are [remote.md → Alerts](../remote.md#alerts), and the port forward notice is [remote.md → Port auto-forwarding](../remote.md#port-auto-forwarding).

## Handlers

A handler is a user command spawned when a notification matches. `[[notifications.handler]]` entries run in config order, and the `[notifications].command` key is shorthand for one more handler with an empty `when`, appended last (`NotificationsPrefs::effective_handlers`). With `enabled = false`, `spawn_notify_handlers` spawns nothing.

### Matching

`NotifyCondition::matches` ANDs the clauses that are present, and an empty `when` matches every notification.

| Clause | Matches when |
| --- | --- |
| `kind` | The notification's kind is listed: `waiting`, `failed`, `paused`, `success`, `coalesced`, `reminder`, `loop_disabled`, `link_lost`, `link_restored` (`loop_paused` is accepted as an alias of `loop_disabled`) |
| `worktree` | Some agent's worktree, its branch or else its path, matches a glob |
| `handle` | Some agent's handle matches a glob, falling back to the agent kind for an agent without one. A leading `@` in the pattern is stripped |

For a coalesced notification each clause may be satisfied by a different agent. A notification that names no agent (`reminder`, link alerts, `loop_disabled`) never matches a handler with a `worktree` or `handle` clause.

### Templates

Handler commands and the `title` and `body` templates substitute `{{name}}` from a closed set. In a handler command each value is shell-quoted with `shlex`, so a template writes variables bare as arguments. The `title` and `body` templates substitute plain text.

| Variable | Value |
| --- | --- |
| `kind` | The notification kind |
| `agent`, `handle` | Agent handles, joined with `, ` for several agents |
| `count` | Number of agents named |
| `unread` | Unread count for reminders, empty otherwise |
| `status`, `worktree`, `task`, `pane`, `root` | The single agent's reached status, worktree, task, pane id, and worktree path, empty when the notification names zero or several agents |
| `title`, `body` | The rendered banner text, in handler commands only |

### Validation

`NotificationsPrefs::validate` rejects an unknown variable name, `{{title}}` or `{{body}}` inside the `title` and `body` templates, an empty handler command, and an invalid glob. Each loader treats the error differently:

| Loader | On an invalid `[notifications]` table |
| --- | --- |
| Strict load (`MachineConfig::load`) | Fails with the error |
| Lenient load (`MachineConfig::load_lenient`): the sidebar, `rimz loop`, `notify-test`, the remote supervisor | Logs a warning and uses the built-in defaults, which have no handlers and `enabled = true` |
| The per-file health check (`config::broken_machine_files`): room entry and `rimz doctor` | Reports the error, even beside a parse-broken theme file, and room entry opens with a warning. It validates the table only when `enabled = true` |

A disabled table with an invalid template is therefore reported nowhere, yet the lenient load replaces it with defaults that are enabled.

### Environment

Every handler receives these variables, set to the empty string when the value is unavailable, except `RIMZ_NOTIFY_UNREAD`.

| Variable | Value |
| --- | --- |
| `RIMZ_NOTIFY_TITLE`, `RIMZ_NOTIFY_BODY` | The rendered banner text |
| `RIMZ_NOTIFY_KIND` | The notification kind |
| `RIMZ_NOTIFY_AGENT` | The agents' row labels (task or prompt, else handle and short id), joined with `, `. This differs from `{{agent}}` |
| `RIMZ_NOTIFY_PANE`, `RIMZ_NOTIFY_ROOT` | The single agent's pane id and worktree path |
| `RIMZ_NOTIFY_ASK` | The open ask id, for a single-agent `waiting` notification from the host |
| `RIMZ_NOTIFY_UNREAD` | The reminder's scope count. Set only on reminders, and not cleared otherwise, so another notification's handler sees whatever value the spawning process inherited |

### Process

`spawn_notify_handlers` runs each rendered command as `sh -c` with stdin, stdout, and stderr on `/dev/null`, hands the child to the global reaper (`child_process::spawn_detached_reaped`), and does not wait. A render or spawn failure logs at debug and skips that handler. Handlers inherit no hook stdout, so they can never write into a hook's decision channel.

### Trust

Handlers live only in the per-machine `~/.rimz/config.toml`, never in a project `.rimz/config.toml`, and sit outside the trust hash. They are personal routing that often carries push credentials, and a cloned repository cannot supply one. The threat model is [security](../../guide/security.md).

A handler can act on the event as well as relay it. With `RIMZ_NOTIFY_ASK` it can read `rimz asks show <id> --json` and answer through `rimz answer <id> <choice>`, which accepts only the supported answers ([transcript.md → Asks and answers](../harness/transcript.md#asks-and-answers)). The user-facing patterns are [the guide → Handlers that answer](../../guide/notifications.md#handlers-that-answer). A script that reads pane text is reading agent output and must treat it as untrusted.

## Configuration

All keys live in `[notifications]` of `~/.rimz/config.toml` (`NotificationsPrefs`). The user-facing description is [configuration → Notifications](../../guide/configuration.md#notifications).

| Key | Default | Effect |
| --- | --- | --- |
| `enabled` | `true` | `false` stops host pushes, reminders, link alerts, renderer bytes, and handlers, and makes `notify-test` refuse. Unread still opens |
| `triggers` | `["waiting", "failed"]` | Statuses whose new episode may push: any of `waiting`, `failed`, `paused`, `success` |
| `desktop` | `"auto"` | `auto` writes OSC except under Zellij, `osc` always, `off` never |
| `sound` | `"bell"` | `bell` writes BEL, `off` writes none (and so no tab marker) |
| `suppress_focused` | `true` | Skips pushes and reminders for panes in `viewed_panes` |
| `debounce_ms` | `5000` | Minimum gap between host pushes for one agent |
| `coalesce_ms` | `1000` | Window that batches host pushes into one, where `0` flushes on each final fold |
| `remind_secs` | `60` | Reminder interval, where `0` disables reminders |
| `title`, `body` | unset | Templates for status and `coalesced` banner text |
| `command` | unset | One unconditional handler |
| `[[notifications.handler]]` | none | `name` (optional), `command`, `when` |

RimZ writes no dock badge escape, because badge APIs differ per terminal and OS. A handler that wants a badge reads `RIMZ_NOTIFY_UNREAD` from reminders.

## The trace log

The trace log records what reached delivery, because a tab `[!]` with no matching unread card leaves nothing else behind. It is `audit/notify.log.jsonl` in the workspace state directory (`~/.rimz/ws/<workspace-dir>/`), beside `audit/diag.log.jsonl`, and rotates at the same 1 MiB cap (`disk::retention::ROTATING_LOG_MAX_BYTES`). Records go through `DiagSink::trace_notify`, which is never rate-limited, and no correctness path reads them. Each record is an envelope (`rimz.notify_trace.v1`, build id, workspace id, session name, renderer instance id when a renderer wrote it, `at_ms`) around one event.

| `kind` | Writer | Fields |
| --- | --- | --- |
| `notification_emitted` | The host, per delivered agent notification or reminder | `notification_kind`, `agents` (`kind`, `agent_id`, `label`, `pane_id`, `new_status`), `panes`, `unread_count` |
| `bell_ring` | A renderer, per `Notify` event it handles | `notification_kind`, `fired`, `recheck_unread`, `panes`, `suppressed` (`notifications_disabled`, `no_own_view`, `daemon_view`, `pane_not_in_view`, `not_unread`) |
| `unread_marked` | The host on reconcile opens; the renderer and `rimz sidebar mark-unread` on manual opens | `row_id`, `label`, agent kind and id, `worktree`, `pane_id`, the reached `status`, `episode_ms` |
| `unread_cleared` | The renderer (`focus`, `tab_view`, `mark_read`), `rimz sidebar mark-read` (`mark_read`), the host (`row_gone`) | `row_id`, `label`, agent kind and id, `worktree`, `pane_id`, `cause`, `cleared_at_ms` |

The log has gaps by design. Push policy writes no record for an open it skips (silent, untriggered, focused, debounced, or pending). The host prunes receipt-reached episodes without a record, because the renderer or CLI that wrote the receipt already logged the clear. `rimz loop` and `notify-test` write no `notification_emitted`, though their renderer-side `bell_ring` records land. Remote link alerts write nothing here.

`bell_ring` with `fired: true` records the decision, before any bytes. With `sound = "off"` no BEL follows, and a failed terminal write leaves the record in place.

To trace a stray tab marker, grep the log for the agent's row id or pane and read the timeline. A `bell_ring` with `fired: true` whose only later clear is `unread_cleared` with `cause: row_gone` points at a marker that outlived a row which vanished before anyone looked. A `bell_ring` with `suppressed: not_unread` is the gate refusing to ring a row that no longer needs a look.

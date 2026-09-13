# Notifications

Notifications push the sidebar's attention signal to a user who is not looking at it: a desktop banner, a terminal bell, a reminder, or a command the user configured. They are best-effort. The durable inputs are the store and the unread episode set; a missed notification costs latency, and the row stays unread in the sidebar until someone reads it.

This page owns the push path: who decides a notification, who delivers it on each channel, the reminder loop, handler matching and templates, and the trace log. Unread episodes and the read receipts that clear them are [sidebar.md → Unread and read receipts](./sidebar.md#unread-and-read-receipts). The user-facing pages are [the notifications guide](../../guide/notifications.md) and [configuration → Notifications](../../guide/configuration.md#notifications).

## Where it lives

| Path | Role |
| --- | --- |
| [`sidebar/notify.rs`](../../../crates/rimz/src/sidebar/notify.rs) | `NotificationState` push policy over newly opened episodes, title and body rendering, `spawn_notify_handlers`, and `LinkNotificationState` for the link-health record |
| [`sidebar/unread.rs`](../../../crates/rimz/src/sidebar/unread.rs) | `UnreadEpisodes`: `unread.json`, `reconcile`, and the `OpenedUnread` records the policy consumes |
| [`sidebar_pane/app/fetch.rs`](../../../crates/rimz/src/sidebar_pane/app/fetch.rs) | `evaluate_notifications` and `deliver_notifications`, run by the elected producer on its fetch cycle |
| [`sidebar_pane/app/notify.rs`](../../../crates/rimz/src/sidebar_pane/app/notify.rs) | `emit_terminal_notification`: the renderer's desktop targeting and `bell_decision` |
| [`sidebar_pane/app/remind.rs`](../../../crates/rimz/src/sidebar_pane/app/remind.rs) | `RemindState`: renderer-local unread reminders |
| [`osc.rs`](../../../crates/rimz/src/osc.rs) | OSC 777 and BEL bytes, the tmux DCS wrap, and the terminal-local variant for processes outside a sidebar pane |
| [`config/notifications.rs`](../../../crates/rimz/src/config/notifications.rs) | `NotificationsPrefs`, `NotificationKind`, handler conditions, template rendering and validation |
| [`diag/notify.rs`](../../../crates/rimz/src/diag/notify.rs) | The `notify.log.jsonl` record schema |

## Channels

| Channel | Mechanism | Crosses SSH | tmux | Zellij |
| --- | --- | --- | --- | --- |
| In-band attention | Sidebar rows, ranking, unread emphasis | n/a | yes | yes |
| Desktop banner | OSC 777 written by a renderer, DCS-wrapped under tmux | yes | yes | off under `desktop = "auto"`, because Zellij drops notification OSCs |
| Sound and tab marker | BEL written by a renderer | yes | yes | tab `[!]` marker; audibility is the terminal's |
| Handlers | `sh -c` commands from `[[notifications.handler]]` | command-defined | yes | yes |

The sidebar is the authoritative surface; every other channel mirrors it. Ghostty, iTerm2, and WezTerm turn OSC 777 into a native desktop banner. Handlers are the portable route: they reach a push service, a detached room, or a Zellij user whose terminal never sees the OSC.

## Who emits which kind

Five code paths construct a notification, and each sets its `NotificationKind`. Handlers match on that kind, and `RIMZ_NOTIFY_KIND` carries it.

| Kind | Emitter | Channels |
| --- | --- | --- |
| `waiting`, `failed`, `paused`, `success`, `coalesced` | The elected producer, over newly opened unread episodes ([the producer](#the-producer)) | Handlers, then `SidebarEvent::Notify` to every renderer for OSC and bell |
| `reminder` | Each renderer, over its own unread scope ([reminders](#unread-reminders)) | The renderer's own OSC and bell, and handlers |
| `link_lost`, `link_restored` | The local `rimz remote connect` supervisor ([remote link alerts](#remote-link-alerts)) | OSC and BEL on the supervisor's stderr, and handlers |
| `loop_disabled` | `rimz loop` when a task auto-disables after consecutive failed fires (`notify_loop_disabled` in `cli/loop_cmd/run.rs`) | Handlers, then a `Notify` event with no panes |
| any | `rimz sidebar notify-test <target>`, a hidden verb in `cli/sidebar.rs` (`--kind`, default `waiting`; `--force-bell`; `--no-command`) | Handlers, then a `Notify` event for the target rows' panes |

Only the producer path applies triggers, debounce, coalescing, and focus suppression, and only the producer writes `notification_emitted` trace records.

## The producer

The elected sidebar producer is the one process that decides agent notifications, so a room with several sidebars pushes each event once. Election is [state.md → Renderers, the producer, and consumers](./state.md#renderers-the-producer-and-consumers).

Each fetch cycle, `evaluate_notifications` loads `unread.json` and the merged read receipts and calls `UnreadEpisodes::reconcile`. Reconcile prunes episodes a receipt reaches (silently) and episodes whose row has left the snapshot (an `unread_cleared` record with `cause: row_gone`), then opens an episode for every row whose displayed status needs a look and that has no open episode and no receipt reaching its `last_activity`. The returned `OpenedUnread` list is the only input to push policy. An episode opened by `rimz sidebar mark-unread` or the renderer's `m` key is written directly to `unread.json`, never appears in that list, and pushes nothing.

When `unread.json` is absent at load, every open in that pass is marked `silent`. Attaching to a busy room therefore renders its current attention rows unread without a burst of banners. Once the file exists, the durable set also dedupes across producer handoff and renderer restart: an episode already recorded does not open again.

`NotificationState::evaluate` then applies the user's policy to each opened episode, in this order:

1. With `enabled = false`, drop everything pending and push nothing. Episodes still open, so the sidebar still shows unread.
2. Skip a silent open, and a status outside `triggers` (default `waiting` and `failed`).
3. With `suppress_focused`, skip a row whose pane is in the snapshot's `viewed_panes`. `viewed_panes` comes from live mux focus, so this is a conservative visibility hint with no durable record behind it.
4. Skip an agent notified less than `debounce_ms` ago. Debounce is keyed by agent kind and session id, and a key is forgotten once its row leaves the snapshot.
5. Skip an agent already pending, then add it to the pending batch with its open ask id, if any.

The first addition to an empty batch starts the coalesce window. The batch flushes when `coalesce_ms` has passed (at once when it is 0). A pending entry whose row is no longer unread by then is dropped before the flush, so a row read within the window pushes nothing. A batch of one becomes a notification of the agent's status kind, with built-in text such as `RimZ: <label> needs you`. A larger batch becomes one `coalesced` notification titled `RimZ: <N> agents need attention`, its body joining `<label>: <status>` pairs with ` | `. Flushing stamps every batched agent's debounce clock.

`[notifications].title` and `.body` then replace the built-in text for the status kinds and `coalesced`; reminders, link alerts, and `loop_disabled` keep their built-in text. A template that fails to render leaves the built-in text in place.

`deliver_notifications` sends each notification three ways, in order: it spawns the matching handlers, appends `notification_emitted` to the trace log, and broadcasts `SidebarEvent::Notify` with the title, body, the agents' pane ids, `recheck_unread: true`, and the kind. The producer writes no terminal bytes itself; its own renderer receives the broadcast like every other.

## The renderer

A renderer turns a `Notify` event into terminal bytes in `emit_terminal_notification`, outside the draw cycle. It decides the desktop banner and the bell separately.

**The bell is bound to current unread attention.** A BEL sets a tab or window marker the renderer cannot retract, so the marker must point at a row that still needs a look. `bell_decision` checks, in order:

| Check | Result when it fails |
| --- | --- |
| The renderer has an own view (it sits in a tab with working panes) | `no_own_view` |
| The view is not a daemon-only view, whose siblings are infrastructure panes | `daemon_view` |
| A target pane is one of the view's working panes | `pane_not_in_view` |
| With `recheck_unread`, a target pane in the view maps to a row with `SidebarRow::unread` set | `not_unread` |

A bell that passes writes BEL when `sound = "bell"`. Because the unread bit stays set until a read receipt reaches the episode, the tab marker and the unread card clear together when the user looks. A `Notify` event with no panes (`loop_disabled`) never rings. `notify-test --force-bell` and reminders send `recheck_unread: false`.

**The desktop banner is a reachability channel.** Under tmux, every renderer with an own view writes the DCS-wrapped OSC 777, so the banner reaches the active client stream even when the agent sits in a background window. Under Zellij, only a renderer whose view holds a target pane writes it, and only with `desktop = "osc"`. `desktop = "auto"` skips desktop OSC under Zellij (`mux::drops_desktop_osc`), and `off` skips it everywhere. The text passes through `osc_text`, which strips control bytes and turns `;` into `:`.

tmux forwards the wrapped payload (`ESC P tmux; ... ESC \`, inner escapes doubled) only when `allow-passthrough` is on, and RimZ turns it on for its rooms by default (`[tmux] allow_passthrough`). Passthrough from an inactive pane and delivery to a detached session are up to the multiplexer and client, so handlers are the deterministic path for a user who is away from the terminal.

When a `Notify` event writes any bytes, the renderer restarts its reminder clock.

## Unread reminders

Reminders are renderer-local and re-ring actionable rows the user has not read. Each renderer runs `RemindState::maybe_remind` on its serve loop, independent of the producer, and no reminder is broadcast as a `Notify` event.

The reminder scope is the renderer's unread rows whose status is `waiting` or `failed` (`AgentStatus::is_actionable`); unread `paused` and `success` rows stay emphasized in the sidebar but never remind.

- A pane-backed row counts when its pane is one of the view's working panes.
- A paneless row counts when the view has working panes that belong to rows in the same worktree path; those panes become the targets. A paneless row with no such pane in any view reminds nowhere and relies on the sidebar and on the producer's handlers.
- With `suppress_focused`, a pane in `viewed_panes` neither counts nor serves as a paneless row's target.

The clock arms when the scope first becomes non-empty or when a `Notify` event writes bytes, and a reminder fires `remind_secs` (default 60) after the later of the arming and the previous reminder. An empty scope, `enabled = false`, or `remind_secs = 0` clears the clock.

A reminder writes through `emit_terminal_notification` with `recheck_unread: false`, since its scope is already unread; the daemon-view and in-view checks still apply. It then spawns matching handlers with kind `reminder`, title `RimZ: <N> unread rows need you`, and `RIMZ_NOTIFY_UNREAD` set to the count. Pane ownership keeps renderers from double-counting a pane-backed row; a paneless row whose worktree has working panes in two views is counted, and its handlers spawned, by both.

## Remote link alerts

The local `rimz remote connect` supervisor alerts on link loss itself, because a dead link cannot carry the remote sidebar's output. A confirmed transport loss emits `link_lost` and the recovery emits `link_restored`: OSC and BEL on the supervisor's stderr when stderr is a terminal (`osc::local_terminal_notification_bytes`, which detects tmux or Zellij from the environment and honours `desktop` and `sound`), plus matching handlers. A probe blackout writes the terminal bytes only and spawns no handler. No link alert is broadcast as a `Notify` event.

A link that is degraded but still passing bytes raises no bell, OSC, or handler; the footer badge shows it, and `LinkNotificationState` writes a `link_alert` diagnostic at each episode edge. The edge rules, hold times, and the supervisor's outage actions are [remote.md → Alerts](../remote.md#alerts).

## Handlers

A handler is a user command spawned when a notification matches. `[[notifications.handler]]` entries run in config order, and the `[notifications].command` key is shorthand for one more handler with an empty `when`, appended last (`NotificationsPrefs::effective_handlers`).

**Matching.** `NotifyCondition::matches` ANDs the clauses that are present, and an empty `when` matches every notification.

| Clause | Matches when |
| --- | --- |
| `kind` | The notification's kind is listed: `waiting`, `failed`, `paused`, `success`, `coalesced`, `reminder`, `loop_disabled`, `link_lost`, `link_restored` (`loop_paused` is accepted as an alias of `loop_disabled`) |
| `worktree` | Some agent's worktree, its branch or else its path, matches a glob |
| `handle` | Some agent's handle or role matches a glob; a leading `@` in the pattern is stripped |

For a coalesced notification each clause may be satisfied by a different agent. A notification that names no agent (`reminder`, link alerts, `loop_disabled`) never matches a handler with a `worktree` or `handle` clause.

**Templates.** Handler commands and the `title` and `body` templates substitute `{{name}}` from a closed set. `NotificationsPrefs::validate` rejects an unknown name, `{{title}}` or `{{body}}` inside the `title` and `body` templates themselves, an empty handler command, and an invalid glob. A strict config load fails on that error. The sidebar, `rimz loop`, and the remote supervisor load leniently (`MachineConfig::load_lenient`): they log a warning and run with the built-in `[notifications]` defaults, which have no handlers. In a handler command each value is shell-quoted with `shlex`, so a template writes variables bare as arguments.

| Variable | Value |
| --- | --- |
| `kind` | The notification kind |
| `agent`, `handle` | Agent handles or roles, joined with `, ` for several agents |
| `count` | Number of agents named |
| `unread` | Unread count, for reminders; empty otherwise |
| `status`, `worktree`, `task`, `pane`, `root` | The single agent's reached status, worktree, task, pane id, and worktree path; empty when the notification names zero or several agents |
| `title`, `body` | The rendered banner text; handler commands only |

**Environment.** Every handler receives these variables; an unavailable value is the empty string.

| Variable | Value |
| --- | --- |
| `RIMZ_NOTIFY_TITLE`, `RIMZ_NOTIFY_BODY` | The rendered banner text |
| `RIMZ_NOTIFY_KIND` | The notification kind |
| `RIMZ_NOTIFY_AGENT` | The agents' row labels (task or prompt, else handle and short id), joined with `, `; this differs from `{{agent}}` |
| `RIMZ_NOTIFY_PANE`, `RIMZ_NOTIFY_ROOT` | The single agent's pane id and worktree path |
| `RIMZ_NOTIFY_ASK` | The open ask id, for a single-agent `waiting` notification from the producer |
| `RIMZ_NOTIFY_UNREAD` | Set only on reminders: the unread actionable count |

**Process.** `spawn_notify_handlers` runs each rendered command as `sh -c` with stdin, stdout, and stderr on `/dev/null`, hands the child to the global reaper (`child_process::spawn_detached_reaped`), and does not wait. A render or spawn failure logs at debug and skips that handler. Handlers inherit no hook stdout, so they can never write into a hook's decision channel.

**Trust.** Handlers live only in the per-machine `~/.config/rimz/config.toml`, never in a project `.rimz/config.toml`, and sit outside the trust hash: they are personal routing that often carries push credentials, and a cloned repository cannot supply one. The threat model is [security](../../guide/security.md).

A handler can act on the event as well as relay it. With `RIMZ_NOTIFY_ASK` it can read `rimz asks show <id> --json` and answer through `rimz answer <id> <choice>`, which accepts only the supported answers ([transcript.md → Asks and answers](../harness/transcript.md#asks-and-answers)); the user-facing patterns are [the guide → Handlers that act](../../guide/notifications.md#handlers-that-act-not-just-alert). A script that reads pane text is reading agent output and must treat it as untrusted.

## Configuration

All keys live in `[notifications]` of `~/.config/rimz/config.toml` (`NotificationsPrefs`). The user-facing description is [configuration → Notifications](../../guide/configuration.md#notifications).

| Key | Default | Effect |
| --- | --- | --- |
| `enabled` | `true` | `false` stops producer pushes, reminders, and link alerts; unread still opens |
| `triggers` | `["waiting", "failed"]` | Statuses whose new episode may push; any of `waiting`, `failed`, `paused`, `success` |
| `desktop` | `"auto"` | `auto` emits OSC except under Zellij, `osc` always, `off` never |
| `sound` | `"bell"` | `bell` writes BEL, `off` writes none (and so no tab marker) |
| `suppress_focused` | `true` | Skips pushes and reminders for panes in `viewed_panes` |
| `debounce_ms` | `5000` | Minimum gap between producer pushes for one agent |
| `coalesce_ms` | `1000` | Window that batches producer pushes into one; `0` flushes each cycle |
| `remind_secs` | `60` | Reminder interval; `0` disables reminders |
| `title`, `body` | unset | Templates for status and `coalesced` banner text |
| `command` | unset | One unconditional handler |
| `[[notifications.handler]]` | none | `name` (optional), `command`, `when` |

RimZ writes no dock badge escape, because badge APIs differ per terminal and OS; a handler that wants a badge reads `RIMZ_NOTIFY_UNREAD` from reminders.

## The trace log

Every notification decision appends to `notify.log.jsonl` in the workspace state directory (`$XDG_STATE_HOME/rimz/workspaces/<id>/`), because a tab `[!]` with no matching unread card leaves nothing else behind. The log sits beside `diag.log.jsonl` and rotates at the same 1 MiB cap (`NOTIFY_LOG_MAX_BYTES`). Records go through `DiagSink::trace_notify`, which is never rate-limited, and no correctness path reads them. Each record is an envelope (`rimz.notify_trace.v1`, build id, workspace id, session name, renderer instance id when a renderer wrote it, `at_ms`) around one event.

| `kind` | Writer | Fields |
| --- | --- | --- |
| `notification_emitted` | The producer, per delivered notification | `notification_kind`, `agents` (kind, id, label, pane, `new_status`), `panes` |
| `bell_ring` | A renderer, per `Notify` event and per reminder | `notification_kind`, `fired`, `recheck_unread`, `panes`, `suppressed` (`no_own_view`, `daemon_view`, `pane_not_in_view`, `not_unread`) |
| `unread_marked` | The producer on reconcile opens; the renderer and `rimz sidebar mark-unread` on manual opens | `row_id`, `label`, agent kind and id, `worktree`, `pane_id`, the reached `status`, `episode_ms` |
| `unread_cleared` | The renderer (`focus`, `tab_view`, `mark_read`), `rimz sidebar mark-read` (`mark_read`), the producer (`row_gone`) | `row_id`, `label`, agent kind and id, `worktree`, `pane_id`, `cause`, `cleared_at_ms` |

The producer prunes receipt-reached episodes without a record, because the renderer or CLI that wrote the receipt already logged the clear. Notifications from the remote supervisor, `rimz loop`, and `notify-test` write no `notification_emitted` record; their renderer-side `bell_ring` records still land.

To trace a stray tab marker, grep the log for the agent's row id or pane and read the timeline. A `bell_ring` with `fired: true` whose only later clear is `unread_cleared` with `cause: row_gone` is a marker that outlived a row which vanished before anyone looked. A `bell_ring` with `suppressed: not_unread` is the gate refusing to ring a row that no longer needs a look.

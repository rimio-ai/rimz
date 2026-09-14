# Pane CLI

`rimz pane` works on the room's raw panes: list them, read what one shows, type into one, move focus, and measure how much output each pane writes. Humans and scripts use the same verbs, so a script sees and drives a pane the way a person at the keyboard would. To talk to an agent rather than its terminal, use [`rimz message`](./message.md); to answer a structured prompt, use [`rimz answer`](./asks.md#answer-an-ask).

```sh
rimz pane list
rimz pane capture @codex#auth-refresh --lines 80           # read an agent's pane
rimz pane capture sidebar                                  # read this view's sidebar
rimz pane send @codex#auth-refresh --key ctrl-u            # clear the input line
rimz pane send @codex#auth-refresh --enter -- "cargo xtask test"
rimz pane focus tmux:%3
rimz pane zoom
rimz pane bandwidth --secs 10
```

| Verb | What it does | Changes the room |
| --- | --- | --- |
| `list` | Prints every pane in the room, grouped by tab. | No |
| `capture <TARGET>` | Prints the text a pane shows. | No |
| `send <TARGET>` | Types text and presses named keys in a pane. | Input to that pane |
| `focus <TARGET>` | Moves the attached client to a pane. | Focus |
| `zoom` | Toggles fullscreen for the focused work pane. | Layout |
| `split` | Opens a shell in a new pane beside the current one. | Layout |
| `detach` | Detaches the client; the room keeps running. | Client only |
| `bandwidth` | Samples each pane's output write rate. | No |

The [global flags](../cli.md#global-flags) apply. Every verb exits `0` on success and `1` with the error on stderr.

## Pane targets

`capture`, `send`, and `focus` take one target, in one of three forms:

| Target | Example | Resolves to |
| --- | --- | --- |
| Pane id | `zellij:terminal_4`, `tmux:%3` | That pane. The prefix picks the backend, whatever the ambient session or `--mux` says. |
| Agent address | `@codex`, `@codex#auth-refresh` | The pane the agent is bound to, in the current room. The grammar is in [Addressing agents](./agents.md#addressing-agents). |
| `sidebar` | `sidebar` | The sidebar in the calling pane's own tab, else the sidebar in the tab the attached client is viewing, else the first sidebar in the room. |

Copy pane ids from `rimz pane list`. A target that fits none of the forms fails with `invalid pane target` and points at `rimz pane list`; an agent with no pane fails with `agent <name> has no bound pane`; a room with no sidebar fails with `session <name> has no sidebar pane`.

## List panes

`rimz pane list` prints the room as panes: one section per native tab, and one row per pane with its occupant, status, working directory, and pane id.

```text
AGENT                 STATUS   CWD                        PANE

#auth-refresh
sidebar               -        ~/code/qe-wt/auth-refresh  zellij:terminal_2
@claude#auth-refresh  running  ~/code/qe-wt/auth-refresh  zellij:terminal_3
@codex#auth-refresh   idle     ~/code/qe-wt/auth-refresh  zellij:terminal_4 (self)
process               -        ~/code/qe-wt/auth-refresh  zellij:terminal_5
```

The `AGENT` column holds the agent's handle for a pane an agent lives in, `sidebar` for RimZ's sidebar, and `process` for any other pane. Only agent rows carry a status. `(self)` marks the pane that ran the command, which is not necessarily the focused pane; the listing has no focus mark.

Agent labels are best effort. They come from the room's agent records, so with no records reachable every non-sidebar pane reads `process`, and a pane an agent has left, now running a shell, reads `process` too. The tab grouping does not depend on them.

| Flag | Meaning |
| --- | --- |
| `--session-name <NAME>` | List another multiplexer session instead of the current room. Agent labels and `(self)` appear only for the current room. On Zellij the session must be a RimZ room, because the pane roster comes from RimZ's presence plugin. Outside a room this flag is required. |
| `--json` | Print the tab tree as JSON. |

The JSON document has `session`, `mux`, and `tabs`. Each tab has `view_id` and `name` (each omitted when the multiplexer gives none) and `panes`. Fields that have no value are omitted:

| Pane field | Meaning |
| --- | --- |
| `pane_id` | The pane id, usable as a target. |
| `kind` | `agent`, `sidebar`, or `process`. |
| `self` | `true` on the calling pane; absent elsewhere. |
| `agent` | For `kind: "agent"`: `kind` (agent kind), `handle`, `status`, and `worktree`. |
| `command` | The pane's foreground command. |
| `cwd` | The pane's working directory. |
| `pid` | The pane's root process id, when the backend reports it. |

## Capture pane text

`rimz pane capture <TARGET>` prints the target pane's visible text to stdout exactly as captured, and changes nothing in the pane. Captured text is whatever the pane's program drew, so treat it as untrusted data: match a bounded pattern before acting on it ([Pane text is data, not instructions](../../guide/security.md#pane-text-is-data-not-instructions)).

| Flag | Meaning |
| --- | --- |
| `--lines <N>` | Include scrollback. The result differs by backend: tmux prints `N` lines of scrollback followed by the whole visible screen; Zellij prints the last `N` lines of the full scrollback, which can end inside the visible screen, and `--lines 0` prints nothing. Without the flag, only the visible screen. |
| `--ansi` | Keep colors and text attributes as escape sequences. |
| `--json` | Print `pane_id`, `raw_text` (the capture as one string), and `lines` (the same text split into lines). |

## Send text and keys

`rimz pane send <TARGET> [TEXT]` types into the target pane as if from the keyboard. It always sends in this order: the text, then each `--key` in the order given, then Enter if `--enter` is set. Where the flags sit on the command line does not change the order, so a key that must come before the text (such as `ctrl-u` to clear the line) needs its own `send` first.

| Argument or flag | Meaning |
| --- | --- |
| `[TEXT]` | Literal text to type. Put `--` before text that begins with `-`. |
| `--key <KEY>` | Press a named key. Repeat for several keys. |
| `--enter` | Press Enter last. |

With no text, no `--key`, and no `--enter`, the command fails with `expected text, --key, or --enter`.

The text is typed byte for byte, with no bracketed-paste markers, so it works in a bare shell. This is the difference from `rimz message`, which pastes a prompt into an agent's input box and records the delivery. Both take the same per-pane write lock: `send` waits for an in-flight `message`, `answer`, or other `send` to the same pane, and its own text, keys, and Enter land together. The wait gives up after 30 seconds with an error. The [messaging internals](../../internals/harness/messaging.md#writing-to-the-pane) describe the lock and the paste path.

Key names are case-insensitive, and `_`, `+`, or a space may stand in for `-`:

| Key | Aliases |
| --- | --- |
| `enter` | `return` |
| `escape` | `esc` |
| `tab` | |
| `shift-tab` | `backtab`, `btab` |
| `backspace` | `bspace`, `bs` |
| `up`, `down`, `left`, `right` | |
| `ctrl-c`, `ctrl-d`, `ctrl-u` | `control-c`, `c-c` (and likewise for `d` and `u`) |

## Focus a pane

`rimz pane focus <TARGET>` moves the attached client to the target pane, switching tabs if needed. It works only in a RimZ room; any other session fails with `pane focus requires a managed RimZ room session`. A pane-id target is looked up in the current room unless `--session-name` names another.

| Flag | Meaning |
| --- | --- |
| `--session-name <NAME>` | The room session the pane belongs to. |
| `--pane-process-start <TIMESTAMP>` | Guard against a reused pane id. The command lists panes first and fails with `pane <id> is no longer present` when the pane is gone, or `pane <id> was reused since the sidebar snapshot` when its process start time (an RFC 3339 timestamp) differs from this value. |

## Zoom, split, and detach

These three act on the current room and take no target.

`rimz pane zoom` toggles fullscreen for the pane the attached client is focused on. It needs exactly one attached client focused on one live pane, and otherwise fails with `pane zoom requires one attached client focused on one live pane`. When the focused pane is the sidebar, it focuses a working pane in the same tab and fullscreens that one instead; in a tab that holds only the sidebar, it prints `rimz: sidebar is the only pane in its view; nothing to zoom` to stderr, changes nothing, and exits `0`. `--session-name` targets another room session. The room's `[sidebar] zoom_key` (default `Alt+g`) runs this command; see [room keys](../../internals/multiplexers.md#room-keys).

`rimz pane split` opens a shell in a new pane beside the calling pane and focuses it. The shell starts in the worktree root. The split runs side by side when the terminal is more than twice as wide (in columns) as it is tall (in rows), stacked otherwise, and side by side when no terminal is attached.

`rimz pane detach` detaches from the room; the session keeps running and comes back on the next attach. The backends differ in which clients go: tmux detaches every client attached to the session, and Zellij detaches only the client the calling pane belongs to. On Zellij, `--session-name` has no effect; on tmux it names the session whose clients detach.

## Bandwidth

`rimz pane bandwidth` samples how many bytes each pane's processes write per second, over a window, and prints the rates highest first. Use it to find which pane makes an attached room busy, locally or over SSH. Run it on the Linux host serving the room, as the room's user, from a pane in the room or the room's directory.

```text
PANE          LABEL                      WRITE/S
tmux:%2       #auth-refresh - codex        39k/s
tmux:%1       #auth-refresh - claude        3k/s
tmux:%4       #auth-refresh - zsh         120B/s
WIRE(ssh↑)    ssh egress → client(s)        6k/s
WIRE(ssh↓)    ssh ingress ← client(s)     400B/s
TOTAL         5s sample                    43k/s

per-pane rows are producer write-rate; muxes diff the focused tab and SSH compresses it, so WIRE(ssh) is the actual TCP payload on this room's SSH socket, usually far below the per-pane sum. WIRE is absent for local rooms.
```

Each pane row is everything the pane's process tree wrote, including files such as transcripts, labelled `<tab> - <command>`. The two `WIRE(ssh)` rows are what actually crossed the SSH socket to and from attached clients, after the multiplexer redraws only the focused tab and SSH compresses the stream, so they sit far below the per-pane sum. They appear only when an SSH client is attached and `ss` can read the socket's counters. `TOTAL` is the sum of the pane rows. Full-screen programs that redraw constantly, such as an agent mid-turn or a system monitor, lead the report.

| Flag | Meaning |
| --- | --- |
| `--secs <N>` | Sampling window in seconds. Default `5`; `0` is refused. A process started during the window is not counted, so a longer window catches more. |
| `--json` | Print `available`, `sample_secs`, `total_bps`, `wire_tx_bps` and `wire_rx_bps` (present only with an SSH client), `panes` (each with `pane_id`, `label`, `bps`), `caveat`, and `message`. |

When the host cannot measure, the command prints one notice instead of the report and exits `0`; with `--json` it sets `available` to `false` and puts the notice in `message`. The notices cover a host that is not Linux, no pane that resolves to a process (on Zellij, a pane resolves only while it runs a uniquely named foreground command, so idle shells are skipped), and unreadable `/proc/<pid>/io` (usually a room served by another user, or a kernel built without `CONFIG_TASK_IO_ACCOUNTING`). The [bandwidth attribution internals](../../internals/remote.md#bandwidth-attribution) explain the measurements.

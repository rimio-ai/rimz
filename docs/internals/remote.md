# Remote attach and link health

`rimz remote connect` attaches to a room that lives on another host. The local process is an SSH launcher and a link supervisor: it parses the target, compiles an `ssh` command, keeps that connection alive across sleeps and network changes, and measures the link. Everything room-shaped (workspace resolution, session birth, the sidebar, the store, the health gate) runs on the remote host's own `rimz`, and the room paints locally because `ssh -t` carries the terminal.

The [remote guide](../guide/remote.md) shows what a user sees, and the [CLI reference](../reference/cli/remote.md) lists the command surface. The shared browser daemon and its credential are [web.md](./web.md), and the backend contracts behind session attach are [multiplexers.md](./multiplexers.md).

## Where the code lives

Two splits shape the code. The first is local versus remote. The local side owns the SSH child, reconnect decisions, terminal hygiene, the connection panel, link measurement, and port forwards. The remote side owns the room. Neither reaches across: the local process never opens the remote store, and the remote room learns nothing about its supervisor beyond the environment variables the launch snippet exports.

The second split is library versus process. [`crates/rimz/src/remote/`](../../crates/rimz/src/remote/) parses targets, builds [`CommandSpec`](../../crates/rimz/src/mux/command.rs) values, classifies exits, and advances state machines from durations the caller supplies. It spawns nothing, reads no clock, and writes no terminal. Its I/O is limited to two places: `remote/aliases.rs` reads and writes `remote.toml`, and `remote/web.rs` binds loopback ports ([invariants](#invariants-worth-preserving)). [`crates/rimz/src/cli/remote/`](../../crates/rimz/src/cli/remote/) owns every process, timer, thread, and terminal write. A new decision belongs in the library with a unit test, and only its execution belongs in the CLI.

| Path | Owns |
| --- | --- |
| [`remote/mod.rs`](../../crates/rimz/src/remote/mod.rs) | Target grammar, the guarded remote shell snippet, `TermPlan`, `SshAttachPlan` (attach, master, and path-preflight argv), lineage derivation, `ReconnectPolicy`, exit-code constants, and `ReconnectState` with its `Verdict`. |
| [`remote/aliases.rs`](../../crates/rimz/src/remote/aliases.rs) | Saved aliases in `remote.toml`: schema, validation, and atomic CRUD. |
| [`remote/link.rs`](../../crates/rimz/src/remote/link.rs) | The `rimz.link.v1` probe protocol, `ProbeWindow` and `LinkMonitor` accounting, the `SessionLinkState` machine, health-tier classification and badge heat, the sidecar and control-socket paths, and the `-O check` and probe-stream argv. |
| [`remote/reachability.rs`](../../crates/rimz/src/remote/reachability.rs) | `ssh -G` endpoint discovery, TUN-interface classification, and the `AttemptPacer`. |
| [`remote/recovery.rs`](../../crates/rimz/src/remote/recovery.rs) | `RecoveryPanel` checkpoint state, final panel rows and stage enums, panel timing, and the internet checkpoint endpoint. |
| [`remote/forward.rs`](../../crates/rimz/src/remote/forward.rs) | Listener parsing from procfs, the `PortSync` diff state, and the forward and cancel argv. |
| [`remote/web.rs`](../../crates/rimz/src/remote/web.rs) | Web prep, tunnel, and control-forward argv, and local relay port selection. |
| [`remote/setup.rs`](../../crates/rimz/src/remote/setup.rs) | The `rimz remote setup` installer snippet. |
| [`remote/tty.rs`](../../crates/rimz/src/remote/tty.rs) | Termios damage detection, flag repair, the DSR status-reply and supervised-output scanners, and the emulator reset string. |
| [`remote/version.rs`](../../crates/rimz/src/remote/version.rs) | Client/host version-skew classification. |
| [`cli/remote.rs`](../../crates/rimz/src/cli/remote.rs) | The clap surface, alias resolution, and the print, one-shot, and supervise fork. |
| [`cli/remote/supervisor.rs`](../../crates/rimz/src/cli/remote/supervisor.rs) | The supervision loop: background masters, foreground attach, probe threads, reachability workers, forwards, local notifications, and the `LinkSupervisor` shared by terminal attaches and web tunnels. |
| [`cli/remote/outage_ui.rs`](../../crates/rimz/src/cli/remote/outage_ui.rs) | The alternate-screen connection panel and its plain-line fallback. |
| [`cli/remote/relay.rs`](../../crates/rimz/src/cli/remote/relay.rs) | Per-attach stdout relay, bounded output drain, scrollback framing, and screen-aware session-end cleanup. |
| [`cli/remote/web.rs`](../../crates/rimz/src/cli/remote/web.rs) | Web prep, the local auth relay, and tunnel supervision. |
| [`cli/remote/link_stats.rs`](../../crates/rimz/src/cli/remote/link_stats.rs) | The remote-side `rimz remote link-stats ingest` service and listener sampling. |
| [`cli/remote/tty.rs`](../../crates/rimz/src/cli/remote/tty.rs) | `TtyGuard`: local termios snapshot, restore, reply fence, and emulator reset. |
| [`cli/remote/setup.rs`](../../crates/rimz/src/cli/remote/setup.rs), [`cli/remote/list.rs`](../../crates/rimz/src/cli/remote/list.rs) | Install and alias listing handlers. |

Five collaborators outside the module run on the remote host or in the sidebar:

- [`cli/room/attach_exec.rs`](../../crates/rimz/src/cli/room/attach_exec.rs) reads the launch variables when the remote `rimz` launches its mux client. It drives the predecessor reap, writes the room-open sentinel and scroll bracket, and runs the session-loss watchdog (`RemoteRoomWatchdog`).
- [`mux/zellij/reap.rs`](../../crates/rimz/src/mux/zellij/reap.rs) retires an orphaned predecessor client.
- [`cli/room/start_notice.rs`](../../crates/rimz/src/cli/room/start_notice.rs) runs the version-skew gate at room entry.
- [`sidebar/enrich.rs`](../../crates/rimz/src/sidebar/enrich.rs) folds the link sidecar onto the snapshot, and [`sidebar/notify.rs`](../../crates/rimz/src/sidebar/notify.rs) bounds health episodes.
- [`sidebar_pane/render/chrome.rs`](../../crates/rimz/src/sidebar_pane/render/chrome.rs) paints the footer link badge.

## Targets and aliases

A remote target is `[user@]host:<session-or-path>`, a RimZ grammar spelled like `scp`. The suffix after the colon decides which remote command RimZ compiles. An ambiguous suffix is resolved on the remote host, never against the local filesystem.

| Suffix shape | `RemoteSpec` | Resolution | Remote command |
| --- | --- | --- | --- |
| `session:<name>` | `Session` | Explicit session, even when a directory has the same name. | `rimz attach --attach -- <name>` |
| Contains `/`, or starts with `~` | `Path` | Explicit path. A missing directory fails. | `rimz start --attach -- <dir>` |
| Anything else, including `.agents` and bare names | `Auto` | An existing directory under remote `HOME` wins, otherwise session attach. | `rimz start --attach -- <dir>` or `rimz attach --attach -- <name>` |

Every relative path is anchored to remote `HOME`, whatever the SSH startup directory. Terminal attach, web prep, and link probes compile the same `RemoteTarget::exec_snippet`, so a directory target's probe selects the workspace instead of treating its suffix as a session name.

An `Auto` target is resolved again at each launch, reconnect, and probe start. If a same-named directory appears or disappears between those moments, an attach and its link probe can select different workspaces. An explicit `session:` or path target pins the choice.

`RemoteTarget::parse` returns a `RemoteTargetError` whose message carries the expected shape and a fix. Three cases matter before you change the parser:

- A bracketed IPv6 host may open the string or follow the `@` that ends the user prefix. An `@[` after the first colon belongs to the suffix.
- `~` and `~/…` normalize to `$HOME`, which `quote_remote_path` keeps outside the single quotes so the remote shell expands it.
- `~user` is rejected, because the single-quoted snippet would carry it literally into a junk path.

`SshDestination::parse` handles the colon-less `[user@]host` form that `rimz remote setup` accepts.

Aliases persist per machine at `~/.rimz/remote.toml`, one `[[remote]]` table per entry, sorted by name and written with temp-file-plus-rename. Unknown fields fail the load.

| Field | Default | Effect |
| --- | --- | --- |
| `name` | required | 1 to 64 ASCII alphanumerics, `-`, or `_`, never a leading `-`. Checked on every add, update, and load. |
| `target` | required | Parsed through `RemoteTarget::parse` on add and update. Load checks names only, so a hand-edited bad target fails when `rimz remote connect` resolves it. |
| `reconnect` | `true` | `false` hands the link to a single `ssh` run with no supervisor. |
| `resume` | `true` | `false` passes `--no-resume` to the remote room. |
| `mux` | unset | Pins `--mux` for this alias. |
| `auto_forward` | `true` | `false` disables listener forwarding for this alias. |

`resolve_connect` in [`cli/remote.rs`](../../crates/rimz/src/cli/remote.rs) merges the alias with the invocation. An input containing `:` is a raw target and starts from all-true settings. A paired setting flag overrides the saved value only when passed, and the last flag in each pair wins. `--mux` on the command line wins over the alias. `update` (`resolve_update`) uses the same setting merge, keeps an omitted target, and takes `--no-mux` to clear the backend pin.

## The command RimZ runs on the host

Every remote invocation ships one shell word to the remote login shell. The terminal attach (`guarded_snippet`) and the web prep one-shot (`web_snippet`) both build it through `remote_exec_snippet`, in a fixed order:

1. Repair `PATH`, because a non-login shell often lacks `~/.cargo/bin`, `~/.local/bin`, `/opt/homebrew/bin`, and `/usr/local/bin`.
2. Run `command -v rimz`, or print the install fix and exit `127`. The supervisor maps that exit to `rimz remote setup <original-input>`.
3. For a `Path` target, run `test -d`, or name the missing path and exit `67`. The guard lives in the snippet so that one-shot connections, the interactive fallback, and any remote RimZ version refuse a missing explicit path before room birth.
4. Export the environment the remote room reads.
5. `exec` into `rimz`, testing for the directory first for an `Auto` target, so no shell survives between SSH and the room.

The probe stream (`probe_stream_spec` in `link.rs`) repairs `PATH` and execs `rimz remote link-stats ingest` with the same target resolution. It skips the missing-binary and path guards: a probe that cannot start is best-effort and must not print into the user's session.

The exported environment is the whole channel from client to host. The host strips the seven `RIMZ_*` names (`mux::REMOTE_CLIENT_ENV`) at mux server birth, so room panes never inherit the remote client's attach context ([the identity pin](./multiplexers.md#the-identity-pin)). The host's launching `rimz start` and its attach client keep the channel through attach. `TERM` and `COLORTERM` stay as pane capabilities. The web prep snippet exports the subset marked in the last column.

| Variable | Carries | Web prep |
| --- | --- | --- |
| `RIMZ_REMOTE_LINEAGE` | A 16-hex identity for this local user and host plus this target, so a replacement attach can retire its own orphan ([retiring the predecessor](#retiring-the-predecessor-client)). | no |
| `RIMZ_REMOTE_SUPERVISED` | Set when the local reconnect supervisor owns the attach's stdout relay and interprets the session-loss exit. The host writes the private OSC `ESC ] 7717 ; rimz-room-open BEL` once, and flushes it after its mode-1007 bracket and immediately before it launches the mux client, even when stdout is not a terminal or color is off. | no |
| `RIMZ_REMOTE_CLIENT_VERSION` | The local binary's semantic version, for the skew gate. | yes |
| `RIMZ_REMOTE_FORCE_VERSION` | Set by `--force-version`, which downgrades a minor refusal to a warning. | yes |
| `RIMZ_REMOTE_RECONNECT` | Set on retry attempts only. It selects the room's [reconnect posture](#reconnect-posture). | no |
| `RIMZ_OUTER_SCROLL_BRACKET` | Set on a one-shot attach whose local launcher already owns the alternate-scroll bracket. | no |
| `RIMZ_CLIENT_SIZE` | Locally probed `<cols>x<rows>`, which seeds the sidebar when the remote birth has no pty. | yes |
| `COLORTERM` | `truecolor` when the local terminal advertises 24-bit color, which SSH does not forward. Web prep always exports it. | yes |
| `TERM` | Set by the `TermPlan` below. Web prep always exports `xterm-256color`. | yes |

`TermPlan` decides how the remote session resolves the local terminal. `ssh -t` carries `$TERM` across, and a remote mux client aborts when its terminfo entry is missing.

| Plan | When | Snippet |
| --- | --- | --- |
| `Keep` | `$TERM` is unset, empty, or one of the 14 names in `term_needs_terminfo_copy`: `xterm`, `xterm-color`, `xterm-16color`, `xterm-256color`, `screen`, `screen-256color`, `tmux`, `tmux-256color`, `vt100`, `vt102`, `vt220`, `ansi`, `linux`, `dumb`. | Nothing. |
| `Copy` | Any other name, and local `infocmp -x` produced a source. | Export `xterm-256color`, pipe the source through remote `tic -x -` with stdout and stderr discarded, then export the real name on success. |
| `Downgrade` | Any other name, and no usable `infocmp` source. | Export `xterm-256color`. |

### Reconnect posture

Room entry reads its posture once, in `StartPosture::detect` ([`cli/room/mod.rs`](../../crates/rimz/src/cli/room/mod.rs)), and the posture decides which prompts and notices a reconnect replays. A non-empty `RIMZ_REMOTE_RECONNECT` selects `Reconnect`. Without it, terminal stdin selects `Attended` and anything else selects `NoTerminal`.

`Reconnect` skips consent questions, the first-run wizard, and the resume prompt. It also skips version warnings and build drift, the non-repository root notice, untrusted-hook warnings, and restricted or undecided folder-trust notices. Version refusals and the resume, reset, web-daemon, and unresumable reports still print. A reborn room that needs hooks keeps the intro and table, but prints `Not asked on a reconnect` with the pointer to `rimz hooks install` in a room pane and to `rimz doctor` for missing hooks. `NoTerminal` keeps its `No terminal input` notice. A reattach to a live room skips hook detection whatever the posture.

### Version skew

The gate runs on the host, in `report_version_mismatch_notices`, and compares `RIMZ_REMOTE_CLIENT_VERSION` against the host binary. `version::classify` compares the first differing numeric component of the `major.minor.patch` core and ignores prerelease and build suffixes. A client that exports no version produces no notice.

| Skew | Behavior | Exit |
| --- | --- | --- |
| `Match` | Silent. | none |
| `Patch` | Warn and proceed. Silent on reconnect. | none |
| `Unparseable` | Warn and proceed. Silent on reconnect. | none |
| `Minor` | Refuse, unless `--force-version` downgrades it to a warning (silent on reconnect). | `65` (`REMOTE_VERSION_SKEW_EXIT`) |
| `Major` | Refuse. `--force-version` does not apply. | `66` (`REMOTE_VERSION_INCOMPATIBLE_EXIT`) |

Both exits reach the local side as fatal verdicts, and `known_remote_exit_message` names the upgrade fix. Under `RIMZ_REMOTE_SUPERVISED`, the host's refusal names both versions and the minor or major tier without generic setup advice, because the local line carries the fix for the target as typed. Without supervision the host prints the full upgrade advice.

### Retiring the predecessor client

Zellij 0.44 can reuse a client id before the queued removal of the old client has drained, which leaves a reconnect painting into a dead client slot. Before an executable Zellij attach, the remote `rimz` reaps the clients of its own lineage (`reap_remote_zellij_predecessors`).

`remote_lineage` hashes a domain tag plus the length-prefixed local hostname, local user, remote host as typed, spec kind, and spec value with SHA-256, and hex-encodes the first eight bytes. The same inputs always give the same value. A second attach from one machine to one target therefore moves the room, while attachments from two machines with different hostnames or users coexist. The hash sees the target text, never the resolved room: `host:repo` and `host:~/repo` can open one directory under two lineages, and two machines that share a hostname and user share a lineage.

`reap_lineage_clients` selects `zellij attach <session> options ...` processes that carry that lineage, excluding its own process ancestry. It sends `SIGTERM` and escalates to `SIGKILL` after a 500ms grace. A bare user attach is never selected. A predecessor started by an older binary that attached with the creation flag is not selected across an upgrade, and it dies with its SSH link. The reaper then polls `zellij --session <name> action list-clients` until the human-client count falls. That successful reply is the ordering fence: Zellij's screen worker has processed the predecessor's removal before the replacement attach registers. The whole sequence is bounded at two seconds.

The reap is best-effort. It is skipped in four cases: tmux, an attach invoked from inside the target multiplexer (so an explicit nested `--attach` cannot retire the outer client that displays that pane), an empty or missing `RIMZ_REMOTE_LINEAGE`, and platforms without a readable process environment. When the room has a workspace id, each reap records a `client_reaped` diagnostic with the killed pids, the client counts before and after, and settlement or timeout. A reap that does not settle prints `rimz: Zellij predecessor cleanup did not settle; attaching anyway` and attaches.

### The session-loss watchdog

Both tmux and Zellij can exit `0` when their attached session is destroyed, exactly as for a detach, so the exit code alone cannot tell the two apart. When `RIMZ_REMOTE_SUPERVISED` is set, the remote RimZ runs `RemoteRoomWatchdog` beside the mux client:

1. Every two seconds (`REMOTE_ROOM_WATCHDOG_INTERVAL`) it queries `session_liveness`. A `Live` answer arms the watchdog.
2. Once armed, a missing or exited session makes it kill and reap the attach client and exit `68`.
3. When the mux client exits on its own, the watchdog checks once more to cover the race between the last poll and the exit. It exits `68` only if it was armed.

A failed liveness query is unknown and preserves the client's status, so uncertain evidence never overrides an intentional detach. One-shot `--no-reconnect` attaches do not export the variable and keep the mux client's status.

## The connect loop

`supervise_remote` uses `LinkSupervisor::connect` for the first connection and `LinkSupervisor::recover` for every recovery. Both prove the transport out of sight, then hand the proven connection to a visible attach. `LinkSupervisor` owns the master, the held screen, and the outage state across terminal and web rounds.

```text
supervise_remote
     │
     ▼
 LinkSupervisor::connect ... wait_for_master behind the connection panel
     │                       reachability workers pace the attempts
     │                       ssh -M -N -o BatchMode=yes, then -O check
     │
     ├── Connected ───────►  initial Path target? test -d over the socket
     │                            │
     │                            ▼
     │                       start the probe stream, then ssh -t on the socket
     │                            │
     │                            ▼
     │                       Verdict::CleanExit → return to the caller
     │                       Verdict::Fatal     → typed error with mapped message and status
     │                       Verdict::Retry     → LinkSupervisor::recover
     │                       Verdict::Reattach  → new ssh -t on the live master
     │
     └── NeedsInteractive ►  one foreground ssh -t, initial connect only
                             (prompts are usable, and a failure there is fatal)
```

### Proving the transport

The background master proves transport and authentication before anything paints, so the panel never shows an all-healthy frame it cannot back. RimZ launches `SshAttachPlan::master` (`ssh -M -N -o BatchMode=yes`, stderr piped) on the control socket `<runtime home>/rimz/link/link-<pid>.sock` (`link::validated_control_path`, which refuses a path over the socket length limit). It then runs `ssh -S <socket> -O check` every 200ms with a 500ms timeout. On the initial connection, a `Path` target next runs `test -d` over the same socket. Exit `1` is the one conclusive missing-path result, and any other exit defers to the normal attach. The visible `ssh -t` reuses the socket.

`MasterState` in `cli/remote/supervisor.rs` has three variants, and each tick yields one outcome:

| State | Tick outcome | Condition |
| --- | --- | --- |
| `Idle` | enter `Connecting` | An attempt is due (immediately on the initial connection, then when the pacer allows) and the control directory prepares cleanly. |
| `Connecting` | connected | `-O check` succeeds and the panel's minimum display has elapsed. |
| `Connecting` | enter `Ready` | `-O check` succeeds while the panel still owes display time. |
| `Connecting` | failed | The child exits, or `master_deadline` (30 seconds) passes. |
| `Ready` | connected | The panel's release time arrives. |
| `Ready` | failed | The child exits. |

The pinned SSH options keep the attach alive only while the master is. The hidden master pins `ControlPersist=no`, `ConnectionAttempts=1`, and `ClearAllForwardings=yes`, and the attach and web prep clients pin `ControlMaster=auto` with `ControlPersist=no`. Inherited configuration therefore cannot persist the master past its child, multiply its attempts, or add forwards to it. The pins do not cover `ForkAfterAuthentication`, so a user config that sets it to `yes` can send the master to the background, outside the supervised child. Identities, proxies, host-key policy, and authentication stay user-owned. Every attach and master sets `Compression=yes`, `ServerAliveInterval=5`, and `ServerAliveCountMax=3`. The master's `ConnectTimeout` is `master_connect_timeout` (5 seconds), and the visible attach keeps `ConnectTimeout=10`.

### The interactive fallback

A batch-mode master cannot answer a password, two-factor, or host-key prompt. On the initial connection, a master child that exits with stderr that `transport_failure` does not recognize releases the panel for exactly one foreground interactive attach. A master that times out never does. The fallback inherits stderr, so authentication prompts, banners, and host-key warnings stay usable.

A failure in that attach is fatal, so RimZ never loops a password prompt. The exception is an established `255`, which enters recovery. With no confirmed master, `settle` gets no evidence, and the attach is itself the master (`ControlMaster=auto`), so `~.` there ends the whole connection and reads exactly as a drop. Recovery stays batch-only and retries until Ctrl-C.

### The handoff

The handoff keeps the alternate screen and click and wheel mouse reporting alive from panel to multiplexer. When the master is confirmed, the panel turns its checkpoints green and animates the yellow Multiplexer `attaching…` row for the rest of the minimum display. The final frame freezes that row with an arrow, parks the cursor on its symbol cell, and releases raw input without leaving the alternate screen or dropping mouse capture. The attached multiplexer paints directly over that frame, and residual scroll momentum stays a mouse event instead of becoming an arrow key.

`RecoveryPanel::frame` derives each row's final status and detail for the current phase. `Active` selects the spinner (or the frozen handoff arrow), `Waiting` is dimmed, and `Checking` is an undimmed unknown checkpoint. On master confirmation every non-handoff row becomes `Ok` with the server's non-suspect detail, and the handoff row becomes `Active`. The CLI maps these statuses to glyphs and colors without reinterpreting stages or parsing detail text.

### The output relay

Every supervised attach pipes SSH stdout through `OutputRelay` in `cli/remote/relay.rs`, including the interactive fallback and connections too fast to show a panel. The relay decides whether host output is speech for the main screen or the start of the room.

Before the host's room-open sentinel or its first alternate-screen entry, printable text, CR, LF, or TAB counts as speech and releases the held panel. The relay then writes a separator and the host bytes on the main screen, where the text stays selectable in scrollback. Attaches after the first add `reconnected` to the separator. Redirected stdout gets no separator.

The separator (`separator_label`) is `── <target> · <HH:MM>[ · reconnected] `, with the target as typed, padded with `─` to 48 cells or the terminal width, whichever is narrower. A target too wide for the terminal is cut at a cell boundary and ends in `…`, so the time and the reconnect mark show whenever the terminal is wider than them. It starts at column zero and ends with CRLF. Its color comes from the CLI rule palette, and it is plain under `NO_COLOR`.

On a silent connect, the relay strips the room-open sentinel and, when a panel was held and color is on, draws a green ✓ at the panel's parked cursor. The multiplexer takes over without a main-screen flash. Escape-only output is held in order up to 16 KiB, and overflow surfaces it as speech. The sentinel ends the speech watch. Text after it is kept as an 8 KiB escape-free tail until the first alternate-screen entry, so a multiplexer that fails before painting has its error replayed on the main screen instead of lost with the panel.

An attach over a confirmed master also pipes and drains SSH stderr, so control-client diagnostics cannot paint over the frame. `attach_error_summary` filters direct and shared-connection close notices and maps `mux_client` and control-socket errors to `SSH control connection dropped`. A `255` exit over a confirmed master with no remaining diagnostic gets the same summary. That summary carries into recovery or the fatal message. A clean exit with captured stderr, the `~.` escape included, prints `rimz: detached from <host>`.

A local relay failure kills SSH and fails the connect instead of reconnecting.

A one-shot `--no-reconnect` attach runs no relay. It owns the alternate-scroll (mode 1007) save, disable, and restore bracket in the local process and exports `RIMZ_OUTER_SCROLL_BRACKET`, so the remote does not nest a second bracket. Under reconnect supervision the local side sets no bracket, and the remote RimZ owns it.

### Classifying the end

`ReconnectState::settle` turns a finished `ssh` into a `Verdict`, checking rows in this order. `attach_interrupted` checks the Ctrl-C row before `settle` runs. The evidence is `(control alive, lived past gatetime)`, supplied only when a master exists.

| Exit | Evidence | Verdict |
| --- | --- | --- |
| `255` with OpenSSH's `Killed by signal 2.` | any | Explicit Ctrl-C: return to the caller without retrying. |
| `0` | any | `CleanExit`: return to the caller. |
| remote RimZ precondition sentinel (`65`, `66`, `67`, `127`) | any | `Fatal`: surface the version, path, or install fix and keep the status through the typed CLI error. |
| session-loss sentinel (`68`) | any | `CleanExit`: report `remote room on <host> ended` and return. |
| `255` | established, master alive (process running and `-O check` passes) | `CleanExit`: the session closed without an exit status over a live connection, as `~.` does, whatever the gatetime. |
| `255` | established, and the master ended, the check failed, or there is no evidence | `Retry`: enter background recovery. |
| other nonzero | established, and the master ended or the check failed | `Retry`: SSH liveness outranks the attach status. |
| other nonzero | master alive, and the attach lived past gatetime | `Reattach`: replace only the multiplexer client over the existing SSH connection. |
| any other status | anything else | `Fatal`: an immediate remote room or authentication failure. |
| signal death | any | `Fatal`: something killed `ssh` deliberately. |

`MasterGuard::is_alive` requires a running child and a successful `ssh -O check` within `CONTROL_MASTER_CHECK_TIMEOUT`, and any check failure means not alive. The mux listener can close before its process exits during transport shutdown, so a process-only poll could mistake a real drop for `~.`.

A signal-killed remote `rimz` or a remote command exiting `255` can produce the same status over a live master. These also end the connect. The room survives, and the user can run `rimz remote connect` again.

A session counts as established once its link probe receives the first ack, once a master (initial or recovery) is confirmed, or once the foreground attach lives past the gatetime (30 seconds by default). Living past the gatetime is also the evidence that the multiplexer attach ran instead of failing at launch. `ReconnectState` keeps establishment across sessions, and `settle_zombie_kill` records an intentional kill without classifying its signal exit as fatal.

`fatal_session_message` words an unknown fatal status. It names `ssh to <host>` when the attach was not established or died by signal. Otherwise it names `rimz on <host>`, and adds `before the room opened` unless the relay saw the sentinel or an alternate-screen entry. Known version, path, and install failures keep their specific fixes. The local report does not repeat the host's last stdout line.

## Terminal hygiene

`TtyGuard` in `cli/remote/tty.rs` keeps the local terminal usable across every session boundary. At connect it snapshots termios, first repairing a leftover raw tty (`termios_damaged`, `sanitize_flags`). After each SSH session it restores the snapshot.

### The reply fence

Before every replacement attach, `settle_terminal_replies` fences off replies addressed to the dead SSH generation. It puts the tty in raw mode, writes a DSR status query (`ESC[5n`) to stderr, and discards input one byte at a time through the terminal's status answer (`ESC[0n`). Terminals answer queries in order, so every DA1, DA2, or XTVERSION reply for the dead generation comes before the fence reply, and type-ahead after it reaches the replacement byte-exact. Without the fence, tmux passes a duplicate reply to the focused pane as keystrokes. The fence uses DSR because the dead client's outstanding DA1 reply would be indistinguishable from a DA1 fence reply.

The fence has three bounds:

- A terminal that ignores `ESC[5n` costs the full two-second cap (`SETTLE_MAX`) on every replacement attach. RimZ then flushes input and loses the type-ahead from that interval. A reply that arrives after the cap can still reach the replacement client: the cap bounds RimZ's wait, and this residual is accepted.
- When stderr is not a terminal, the query cannot be written. RimZ drains until 250ms of quiet (`SETTLE_QUIET`) and flushes. Ctrl-C ends this quiet-window drain at once, and during a fence it is discarded like any other byte.
- Keystrokes typed during an outage are discarded, because no session owns them.

Exit paths and the initial attach never drain terminal input, so the shell that regains the terminal keeps its type-ahead. The live `backend::tmux::remote_reconnect` integration suite exercises the fence against a real query-producing tmux client.

### Session end

Every session end runs one fixed sequence, in `run_ssh_session` and `RelayOutput::complete`:

1. Drain SSH pipe reads to EOF or a 200ms deadline. The bound does not cover synchronous stdout writes: a consumer that stops reading can block the relay, as it blocked SSH when stdout was inherited.
2. Restore the termios snapshot.
3. Resolve the panel guard's terminal modes without flushing its captured logs. If the host never entered the alternate screen, the guard restores its modes. Otherwise it is disarmed without writing terminal modes.
4. Leave the alternate screen when the relay's tracked screen is alternate or unknown.
5. Write `EMULATOR_RESET`. It disables mouse, focus, and paste reporting and resets the scroll region while it keeps the cursor, with no unconditional alternate-screen leave or cursor move.
6. Replay captured logs to stderr on the main screen from column 0, then any unpainted mux tail behind a separator, then the local end line. A CRLF is supplied wherever those writes leave an unfinished line.

While a panel guard owns the terminal, the tracing writer buffers warnings instead of writing through the frame. When the connect showed a panel, the relay keeps that log capture for the whole session, even after host speech steps off the panel, so step 6 replays it. A connection that lands inside the grace shows no panel and captures nothing, so its warnings write live through the multiplexer's frame. Fatal failures return immediately with their error in scrollback, without waiting for input.

## Reconnect pacing and reachability

`AttemptPacer` decides when the next master attempt may start. It fuses the configured checkpoints under one rule: any positive result means the network is up, the network is down only after every configured checkpoint reports down, and an unknown first result stays optimistic. With no checkpoint configured, the network counts as up.

| Network | Pacing |
| --- | --- |
| Up | `reachable_retry` (2s) until the current wait is `flat_window` (3 minutes) old, then doubling once per minute to `backoff_cap` (30s). |
| Down | `1s, 2s, 3s, …, 10s`, then `20s`, then `30s` for every later attempt. |

`wait_for_master` in `cli/remote/supervisor.rs` builds a fresh pacer for each wait, the initial connect and every recovery. Both ladders and the flat window therefore count from the start of the current wait and start over after every lost attach, however long the link has been flapping. Within a wait, two edges reset both ladders and schedule an immediate attempt: a down-to-up checkpoint transition, and a change in the local route fingerprint. The fingerprint is the source address the kernel selects for a UDP route lookup toward `1.1.1.1:443`, and the lookup sends no packet.

Endpoint discovery (`resolve_dial_plan`) runs once at supervisor startup. `ssh -G -- <destination>` reports the effective `hostname` and `port` after user config applies, and `parse_dial_plan` turns them into a `DialPlan`. A configured `ProxyJump` or `ProxyCommand` yields no plan, because a direct dial would not test the path SSH uses. A failed query or unparseable output also yields none. Without a plan there is no Server checkpoint and no zombie guard, and the internet checkpoint and route fingerprint pace attempts alone. DNS resolution stays per-dial, so a network change can supply a fresh address.

During initial connect and during an outage, the reachability workers run every second (`RIMZ_REMOTE_DIAL_MS`). Each round samples the route fingerprint, dials the endpoint with a two-second TCP timeout (`DIAL_TIMEOUT`), and, while the panel is shown, fetches the internet checkpoint with a one-second HTTP timeout. Before each dial, a packet-free route lookup finds the owning local interface. `is_tun_interface` treats a name starting with `tun`, `utun`, `wg`, or `tailscale`, and any point-to-point interface, as reachable without a dial, because a direct probe can blackhole over such a route while SSH succeeds through it.

These workers drive presentation and pacing and never decide truth. The background master stays the end-to-end proof.

| `ReconnectPolicy` field | Default | Meaning |
| --- | --- | --- |
| `gatetime` | `30s` | Lifetime that makes an unconfirmed session count as established. |
| `reachable_retry` | `2s` | Flat retry delay while the network is up. |
| `flat_window` | `3min` | Age of the current wait at which flat pacing gives way to doubling. |
| `backoff_cap` | `30s` | Ceiling for both ladders. |
| `master_connect_timeout` | `5s` | Per-attempt TCP connect and SSH banner budget. |
| `master_deadline` | `30s` | Total lifetime of one background master attempt. |

## The connection panel

An interactive terminal gets a full-screen panel for the initial connection and for each outage, shown after a 500ms grace (`RECOVERY_GRACE`). `panel_allowed` falls back to plain stderr lines when stdout is not a terminal, `RIMZ_NO_PROGRESS` is `1` or `true`, `TERM` is `dumb`, or an agent owns the terminal.

The panel has four rows, one per pipeline stage. Network rows hold their last settled result across attempts, including a regression to unreachable. The last row stays present throughout, so the centered layout is stable at handoff.

| Row | Checkpoint |
| --- | --- |
| Internet | `GET http://cp.cloudflare.com/generate_204` returning status `204`. |
| Server | The effective SSH endpoint from `ssh -G`. Omitted when there is no dial plan. |
| SSH session | The next master attempt. |
| Multiplexer (`Web tunnel` under `--web`) | Waiting until the master confirms, then `attaching…` (`opening…` for the web tunnel). |

`StageStatus::Suspect` is the one non-obvious status. Once an SSH attempt has failed, a Server row that still answers TCP reads yellow with `answers TCP · SSH failing`, instead of green beside a failing connection. A TUN route reads `via TUN <interface> · TCP check skipped`, and becomes `via TUN <interface> · SSH failing` under the same condition.

Exactly one row animates: the Internet row while waiting for the network, the SSH session row while a master connects, and the last row after confirmation. The active phase, countdown, and final OpenSSH stderr line fold into that row. A dim header carries the attempt number, elapsed time, and `Ctrl-C stops`. The initial wording says `Connecting`, and recovery says `Connection lost`. The block is centered, with left-aligned rows and fixed label columns.

Once shown, the panel stays at least 1.5s (`RECOVERY_MIN_DISPLAY`), so a fast reconnect spends the remaining hold on the attaching animation instead of flashing. A connection that lands inside the grace never shows a panel, a local ✓, or a handoff frame. Fatal failures release the panel and return their error and fix directly to scrollback, after the host startup text the relay surfaced.

Ctrl-C is polled as a terminal event in raw mode while the panel owns the terminal. It kills a pending master, restores the terminal, prints `rimz: connect to <host> stopped` (or `reconnect`) with the last error, and stops the supervisor. Once SSH owns the terminal, OpenSSH reports a remote command interrupted by Ctrl-C as status 255 plus `Killed by signal 2.`, which `attach_interrupted` recognizes before transport classification.

## Link health

When reconnect is enabled, the supervisor runs a long-lived probe stream over the same ControlMaster connection as the attach, so its measurements describe the user's session path instead of an ICMP round trip:

```text
ssh -S <control-sock> -o BatchMode=yes -- <host> '<PATH repair>; exec rimz remote link-stats ingest --session <name>|--dir <path>'
```

The local side writes one JSON line every two seconds (`LINK_PROBE_INTERVAL`), plus a stats-refresh line whenever the displayed RTT changes. The remote `ingest` replies with one JSON ack per line and republishes a sidecar file. A stream that does not start on a confirmed master first waits for `control_check_spec` to succeed: `ssh -S <path>` without `-O check` would open a fresh TCP connection when the socket is absent and measure a link nobody uses.

### Protocol

All three shapes carry the schema version `rimz.link.v1`. `LinkProbe` carries `seq`, `sent_at_ms`, and the stats settled before this line, so the remote file always holds a complete window. `LinkAck` carries `seq` and an optional `ports` array. `LinkStatsFile` carries the remote `received_at_ms`, the SSH client identity (`SSH_CONNECTION`), and the latest stats.

The `ports` field is optional so both skew directions keep working: a remote that never sends it pairs with a local that reads it, and a local that does not read it ignores it. A probe whose version the remote rejects makes `ingest` exit `2`. The local probe loop treats exit `2` or `127` as terminal and stops probing for that session without touching the room.

### Measurement

`ProbeWindow` keeps the latest 30 probes sent (`LINK_WINDOW`), unanswered ones included, and ignores an ack whose probe has already left the window. Loss is the miss percentage over settled probes. Stats-refresh lines are not measurement samples, and the window ignores their acks.

Misses and blackouts start counting only after the first accepted ack. Before it, `LinkMonitor::check_blackout` returns at once: pending probes do not expire, and no blackout fires however long the stream stays silent. After it, a pending probe expires into a miss after the two-second timeout, and a late ack for an already-missed probe is ignored. A session whose stream never acks therefore relies on OpenSSH keepalives alone.

RTT is an EWMA whose smoothing factor adapts. A sample within 8% of the current value uses `alpha = 0.15`, and the factor ramps to `0.60` as the relative deviation approaches 50%, so a real path change moves the number quickly while jitter does not. The displayed value holds until the smoothed value moves at least 8ms, which keeps the badge steady. Each new probe stream discards its first ack sample, because the stream execs a fresh remote `ingest` whose spawn cost would read as latency.

`LinkMonitor` wraps the window and emits three `LinkEvent`s: `FirstAck` the first time any ack lands, `Blackout(duration)` when eight seconds (`LINK_BLACKOUT_AFTER`) pass after the last ack, latched so one outage produces one event, and `Recovered` on the next ack after a latched blackout.

### The probe stream's lifetime

Each attach session starts its own probe thread (`probe_loop`) with a fresh `LinkMonitor`. Within the session, a stream that ends is respawned: after 1s when it had acked, otherwise on a doubling backoff from 1s to 30s. Once three streams in a row have ended without an ack (`PROBE_STREAM_BLACKOUT_FAILURES`), the loop also runs the blackout check between streams, so a stream that keeps dying after an earlier ack still raises its blackout. The thread stops when the session ends, on exit `2` or `127`, or when `RIMZ_REMOTE_PROBE_MS=0` disables probing.

### The session link machine

`SessionLinkState` reduces gatetime, link events, and session boundaries into actions the supervisor renders. Outage state spans reconnects, and session-local state resets through `begin_session`.

| Input | Effect | Action |
| --- | --- | --- |
| `begin_session` | Clears session-local state, remembers whether an outage is open. | none |
| Elapsed reaches gatetime | Marks established. | `Restore`, when the session began during an open outage |
| `FirstAck` | Marks established and confirmed, clears the zombie watch. | `Restore`, when an outage was open |
| `Blackout(d)` | Arms the zombie watch, opens the outage. | `NotifyBlackout(d)`, once per outage |
| `Recovered` | Clears the zombie watch. | `Restore` |
| `transport_lost` | Opens the outage. | `NotifyTransportLoss`, once per outage |
| Zombie watch armed and established | Schedules the next check. | `VerifyZombie` |
| `finish` | Settles an exited child. | none |

`finish` emits nothing on purpose: child exit outranks link presentation, so a session that ends while a blackout notification is queued reports its exit instead of a stall.

### The zombie guard

A suspended laptop or a NAT rebind can leave an SSH child holding the tty on a transport that will never carry another byte. OpenSSH keepalives (`ServerAliveInterval=5`, `ServerAliveCountMax=3`) reach exit `255` in roughly fifteen seconds for a hard loss, but a zombie transport can outlive them.

`verify_zombie` replaces the session only when three conditions hold: the session is established, its probe stayed silent past the blackout threshold, and a fresh endpoint dial succeeds (a TUN route satisfies the dial without one). If any one fails, RimZ waits, because a slow link and a dead link look identical from the blackout alone. When all three hold, RimZ kills the child with `SIGKILL` so the replacement can claim the terminal, and prints `rimz: link to <host> confirmed dead`. The guard needs a dial plan and a nonzero dial cadence, so proxied configurations, `--no-reconnect`, disabled probes, probe version skew, and a stream that never acked rely on OpenSSH keepalive death alone.

### Publication and freshness

`ingest` writes the sidecar `<runtime>/lanes/link-stats.json` (`link::stats_path`, through `RuntimePaths::lane_path`) with temp-file-plus-rename cache semantics, and the sidebar reads it on every enrichment fold. When the probe stream ends, `ingest` removes the sidecar if its `client` still matches this connection's `SSH_CONNECTION`. Age expiry covers hard drops where no shutdown ran. Local rooms never have the file, so their footer shows no badge.

| Age | Freshness | Footer |
| --- | --- | --- |
| Up to 10s | Fresh | The measured badge, or `⇄ remote …` while RTT is still warming. |
| 10s to 120s | Stale | `⇄ remote ?` in the muted tone. |
| Past 120s | Expired | No badge. |

### Two health scales

The module keeps a stepped scale for alerting and a continuous one for the badge. Mixing them up is an easy bug.

`LinkTier` is the stepped scale. The type lives in `ids` beside the other shared classification enums, and the snapshot's `SidebarLinkHealth` record embeds it. `remote::link::link_tier` owns the thresholds and takes the worse of the two axes:

| Axis | Good | Degraded | Bad |
| --- | --- | --- | --- |
| RTT | up to 150ms | 151 to 400ms | above 400ms |
| Loss | 0% | 1% to 10% | above 10% |

`link_badge_heat` is the continuous scale: latency maps linearly over `100..=400ms`, loss over `0..=30%`, and the badge takes the worse axis. The renderer turns that `0.0..=1.0` value into a tone through `Theme::heat_tone` ([theme.md](./theme.md)), which keeps the link module theme-free, and adds bold at the red end so a critical link stays loud without color. A warming badge with no RTT sample returns `None` and paints the neutral resting tone. The badge appends `{n}%` only when loss exceeds 10%.

The scales disagree on purpose: low nonzero loss can open an alert episode while the badge still reads close to green.

### Alerts

Alerts split across two delivery paths, by who can still reach the user. The delivery channels for each event are tabled in [sidebar/notifications.md](./sidebar/notifications.md#remote-link-alerts).

The local supervisor owns outage alerts, because a dead link cannot rely on the remote-rendered sidebar. Confirmed link-lost and link-restored edges emit terminal-local OSC and BEL plus notification handlers. A probe blackout emits the terminal-local signals only, because it is a local stall and not a confirmed drop.

The remote sidebar owns degraded-but-alive links. They surface through the footer badge alone and raise no bell or handler. `LinkNotificationState` in `sidebar/notify.rs` still bounds a health episode for the record: ten seconds of fresh degraded or bad stats opens one, thirty seconds of fresh good stats closes it, and stale stats pause both clocks. Each edge writes a `link_alert` diagnostic carrying the tier, RTT, miss percentage, episode start, and recovery duration.

## Port auto-forwarding

A dev server started in a room pane after attach becomes reachable on the same local port, with no second SSH command. Discovery rides the probe ack, so the feature needs the supervised link and is off under `--no-reconnect` and `--web`.

On the host, `ingest` samples `/proc/net/tcp` and `/proc/net/tcp6` at most every five seconds and attaches the latest `ports` array to each ack. `candidate_ports` keeps a listener in state `0A`, owned by the room user's uid, on port 1024 or above, and bound to a loopback or wildcard address. It reports at most 32 ports, sorted and deduplicated. A host without those procfs files sends no `ports` field, and the connection is otherwise unaffected.

Locally, `PortSync` diffs the reports:

- The first report of the connection becomes a permanent baseline and opens nothing, so services running at attach are never forwarded.
- A port absent from the baseline is opened, up to 16 active forwards.
- A port missing from three consecutive reports is closed.
- An open that fails, because the local port is taken or `ssh -O forward` refuses, is parked until that listener disappears from a report and later returns.

Before it asks the master to forward, `apply_port_actions` binds `127.0.0.1:<port>` itself and releases it, which turns a busy local port into a park instead of a stream of refusals. A bind-failure park is reported to the attached user through the local terminal notification channel, which respects notification preferences and runs no link-event handlers. The notice names the port and tells the user to free it, then stop and restart the server on the host to retry. The forward is `ssh -O forward -L 127.0.0.1:<port>:localhost:<port>` on the live master with a two-second timeout, and `ssh -O cancel` with the same argument closes it, so the local side never listens on a public address.

The baseline and active set live for the whole `rimz remote connect`, across probe-stream and transport replacements. The probe thread of each attach session reopens the active set before reports resume (`reopen_port_forwards`), so a replacement master carries the forwards over. The master's exit tears every forward down at detach. The reopen also runs after a `Reattach`, against the master that still listens on those ports, so the local bind check fails for each one and parks it. Restarting `rimz remote connect` takes a new baseline.

## Web tunnels

`rimz remote connect <target> --web` opens the remote room in a local browser and stays in the foreground supervising the tunnel. The prep payload, the local auth relay, its port selection, and the security boundary belong to [web.md](./web.md#remote-rooms). This section covers only how the tunnel rides the SSH supervision above.

`run_supervised_web` creates a `LinkSupervisor`, which establishes the initial master through `wait_for_master` with a Web tunnel row in place of Multiplexer. `WebRounds` keeps the relay port and target together and runs each round with either a master control forward or an owned tunnel child. Each round runs three steps over the confirmed master:

1. Run remote `rimz web open --print --json` as a non-PTY one-shot with stderr inherited. A prep exit of `255` is a transport failure and enters recovery. `127`, `65`, `66`, and `67` get the same fatal messages as terminal attach. Any other failure aborts.
2. Reserve a fresh ephemeral local port and install `-L 127.0.0.1:<ephemeral>:127.0.0.1:<tunnel_port>` through `ssh -O forward` on the master. The first round binds the user-facing relay port, and later rounds retarget the relay at the new forward.
3. On the first round, print the URL to stdout, open the browser best-effort, and print `rimz: tunnel up` to stderr. Later rounds print `rimz: tunnel to <host> restored`. Then wait on the master until Ctrl-C or transport exit.

Every round returns one exit record (code and port readiness), including a prep transport failure and a readiness exit, and the loop settles it once. The confirmed master or the accepting local port marks a round established, so a transport exit `255` after either proof calls `LinkSupervisor::recover`, which runs `wait_for_master` again behind the panel. Recovery repeats prep so a changed remote port or credential is picked up, and the browser URL stays stable because only the relay's upstream changes. The `-O forward` client exits once the master confirms the listener, so its clean exit is not a detach. The web path runs no probe stream, zombie guard, or port auto-forwarding.

An initial connection that needs interactive authentication runs one round without a master: prep uses a direct connection, and the tunnel is a separate `ssh -N` child (`web_tunnel_spec`). Recovery after it is batch-only. `--no-reconnect` (`run_direct_web`) skips the supervisor and panel entirely and uses the same direct prep and tunnel child. The tunnel pins `ControlMaster=no` and `ControlPath=none` so ambient SSH multiplexing cannot move the forward away from its foreground child, and the command exits when that child ends.

## Bandwidth attribution

`rimz pane bandwidth --secs 5` measures what a room costs its link. It samples the room it runs in and reports each pane's Linux process write-rate, plus the SSH wire-rate when an SSH client is attached. It must run on the Linux host serving the room, as the room's user, and without `sudo`, because privilege escalation resets backend and socket resolution away from the room. The accounting lives with the pane primitives in [`pane/bandwidth.rs`](../../crates/rimz/src/pane/bandwidth.rs), and [`cli/pane/bandwidth.rs`](../../crates/rimz/src/cli/pane/bandwidth.rs) owns sampling and presentation. This page documents it because the `WIRE(ssh)` rows describe the remote transport.

The command resolves the current workspace session, lists panes through the selected backend, pins each pane's root process tree at the start of the window, and reads `/proc/<pid>/io` `wchar` before and after the sleep. For a remote room it also matches the attached mux client's `SSH_CONNECTION` tuple to its socket and reads `ss` TCP_INFO counters (`bytes_acked`, `bytes_received`) over the same window. These print as `WIRE(ssh↑)` for egress to the client and `WIRE(ssh↓)` for ingress.

The two numbers measure different points in the pipeline, which is why the command reports both. Per-pane rows are producer write-rate, including non-pty writes such as transcript files. Between there and the socket, the multiplexer diffs and throttles output to the focused tab, and SSH compresses the payload. `WIRE(ssh)` is what crosses the link, and it is normally far below the per-pane sum.

Three notices replace the report when the host cannot support it: no Linux write-rate counters, panes that resolve to no root process, and unreadable `/proc/<pid>/io` entries. macOS process disk counters stay out on purpose, because they would misrepresent terminal output. `WIRE(ssh)` is omitted for local rooms, for rooms with no attached SSH mux client in the process table, and where `ss` is unavailable.

tmux reports pane root pids natively. Zellij pane pids come from the process table through the same matcher the sidebar metrics path uses, so an active uniquely named foreground command binds while an idle look-alike shell abstains. Because the sampler pins the process tree at the first snapshot, short-lived children born mid-window can escape the sample. A longer window catches persistent high-churn TUIs.

## Test seams

The table lists the module's test overrides. Every other duration is fixed, so a test satisfies it or waits it out. A test passes the two-second DSR fence (`SETTLE_MAX`) at once by answering `ESC[5n` with `ESC[0n`. The fallback quiet window (`SETTLE_QUIET`, 250ms), the panel tick (`PANEL_TICK`, 80ms), and the control-master check interval (`CONTROL_MASTER_CHECK_INTERVAL`, 200ms) are waited out.

| Variable | Controls |
| --- | --- |
| `RIMZ_SSH_BIN`, `RIMZ_INFOCMP_BIN` | Binary overrides. `tests/fixtures/ssh-trace` is the standard shim. |
| `RIMZ_REMOTE_GATETIME_MS` | Establishment threshold. |
| `RIMZ_REMOTE_MASTER_CONNECT_MS`, `RIMZ_REMOTE_MASTER_TIMEOUT_MS` | Background master connect and total deadlines. |
| `RIMZ_REMOTE_REACHABLE_RETRY_MS`, `RIMZ_REMOTE_FLAT_WINDOW_MS`, `RIMZ_REMOTE_BACKOFF_CAP_MS` | Retry pacing. |
| `RIMZ_REMOTE_GRACE_MS`, `RIMZ_REMOTE_MIN_DISPLAY_MS` | Panel grace and minimum display. |
| `RIMZ_REMOTE_PROBE_MS`, `RIMZ_REMOTE_PROBE_TIMEOUT_MS`, `RIMZ_REMOTE_BLACKOUT_MS` | Probe cadence, timeout, and blackout threshold. `RIMZ_REMOTE_PROBE_MS=0` disables probing. |
| `RIMZ_REMOTE_WATCHDOG_MS` | Session-liveness check interval of the supervised attach, read once per attach. The first check is immediate. Only a positive integer in milliseconds replaces the 2s default, and unset, `0`, or invalid values keep it. |
| `RIMZ_REMOTE_DIAL_MS` | Reachability cadence. `0` disables endpoint discovery, every TCP dial, the internet checkpoint, and fingerprint resets. |
| `RIMZ_REMOTE_TUN` | Forces a TUN interface name for route classification. |
| `RIMZ_REMOTE_INTERNET_PROBE` | Replaces the internet checkpoint with an HTTP or HTTPS URL that must return `204`. Empty or `0` disables the row. |
| `RIMZ_PORTS_SWEEP_MS`, `RIMZ_PROC_NET_DIR` | Remote listener sampling cadence and the procfs root. |

## Invariants worth preserving

- Keep `remote/` free of processes, threads, clocks, and terminal writes. Parsing, argv, classification, and state machines belong there with unit tests, and their drivers belong in `cli/remote/`. Two kinds of I/O are admitted. `remote/aliases.rs` loads and saves `remote.toml`. `remote/web.rs` binds loopback listeners in `bind_local_relay` and `reserve_forward_port` to pick free local ports: the relay keeps the listener it bound, and the forward port is probed and released for SSH to bind.
- Prove the transport before painting success. The background master's `-O check` is the proof, and reachability dials are presentation and pacing only.
- Keep the attach alive only while the master is. The pinned `ControlPersist=no`, `ConnectionAttempts=1`, and `ClearAllForwardings=yes` options exist so inherited SSH configuration cannot break that ([the `ForkAfterAuthentication` gap](#proving-the-transport)).
- Require all three guards before killing a session as a zombie: established, blacked out, and independently reachable.
- Restore the terminal on every exit path: snapshot termios at connect, restore after each session, fence terminal replies before each replacement attach, and write the emulator reset at every session end.
- Keep new ack and sidecar fields optional, so each is invisible to a peer on the other side of a version skew.
- Let the remote host own the room. Put a new remote capability behind a `rimz` subcommand the snippet execs, so the local side keeps reaching the host through the one channel it already supervises.

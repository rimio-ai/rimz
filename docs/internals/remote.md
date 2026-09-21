# Remote attach and link health

`rimz remote connect` attaches to a room that lives on another host. The local process is an SSH launcher and a link supervisor: it parses the target, compiles an `ssh` command, keeps that connection alive across sleeps and network changes, and measures the link. Everything room-shaped (workspace resolution, session birth, the sidebar, the store, the health gate) runs on the remote host's own `rimz`, and the room paints locally because `ssh -t` carries the terminal.

The [remote guide](../guide/remote.md) shows what a user sees, and the [CLI reference](../reference/cli/remote.md) lists the command surface. The shared browser daemon and its credential are [web.md](./web.md); the backend contracts behind session attach are [multiplexers.md](./multiplexers.md).

## Where the code lives

Two splits shape the code. The first is local versus remote. The local side owns the SSH child, reconnect decisions, terminal hygiene, the connection panel, link measurement, and port forwards. The remote side owns the room. Neither reaches across: the local process never opens the remote store, and the remote room learns nothing about its supervisor beyond the environment variables the launch snippet exports.

The second split is pure versus process. [`crates/rimz/src/remote/`](../../crates/rimz/src/remote/) is a pure library: it parses targets, builds [`CommandSpec`](../../crates/rimz/src/mux/command.rs) values, classifies exits, and advances state machines from durations the caller supplies. It spawns nothing and reads no clock. [`crates/rimz/src/cli/remote/`](../../crates/rimz/src/cli/remote/) owns every process, timer, thread, and terminal write. A new decision belongs in the library with a unit test; only its execution belongs in the CLI.

| Path | Owns |
| --- | --- |
| [`remote/mod.rs`](../../crates/rimz/src/remote/mod.rs) | Target grammar, the guarded remote shell snippet, `TermPlan`, `SshAttachPlan` (attach, master, and path-preflight argv), lineage derivation, `ReconnectPolicy`, exit-code constants, and `ReconnectState` with its `Verdict`. |
| [`remote/aliases.rs`](../../crates/rimz/src/remote/aliases.rs) | Saved aliases in `remote.toml`: schema, validation, and atomic CRUD. |
| [`remote/link.rs`](../../crates/rimz/src/remote/link.rs) | The `rimz.link.v1` probe protocol, `ProbeWindow` and `LinkMonitor` accounting, the `SessionLinkState` machine, health-tier classification and badge heat, the control-socket path, and the `-O check` and probe-stream argv. |
| [`remote/reachability.rs`](../../crates/rimz/src/remote/reachability.rs) | `ssh -G` endpoint discovery, TUN-interface classification, and the `AttemptPacer`. |
| [`remote/recovery.rs`](../../crates/rimz/src/remote/recovery.rs) | `RecoveryPanel` checkpoint state and stage enums, panel timing, and the internet checkpoint endpoint. |
| [`remote/forward.rs`](../../crates/rimz/src/remote/forward.rs) | Listener parsing from procfs, the `PortSync` diff state, and the forward and cancel argv. |
| [`remote/web.rs`](../../crates/rimz/src/remote/web.rs) | Web prep, tunnel, and control-forward argv; local relay port selection. |
| [`remote/setup.rs`](../../crates/rimz/src/remote/setup.rs) | The `rimz remote setup` installer snippet. |
| [`remote/tty.rs`](../../crates/rimz/src/remote/tty.rs) | Termios damage detection, flag repair, the DSR status-reply scanner, and the emulator reset string. |
| [`remote/version.rs`](../../crates/rimz/src/remote/version.rs) | Client/host version-skew classification. |
| [`cli/remote.rs`](../../crates/rimz/src/cli/remote.rs) | The clap surface, alias resolution, and the print, one-shot, and supervise fork. |
| [`cli/remote/supervisor.rs`](../../crates/rimz/src/cli/remote/supervisor.rs) | The supervision loop: background masters, foreground attach, probe threads, reachability workers, forwards, local notifications, and the `LinkSupervisor` that web tunnels share. |
| [`cli/remote/outage_ui.rs`](../../crates/rimz/src/cli/remote/outage_ui.rs) | The alternate-screen connection panel and its plain-line fallback. |
| [`cli/remote/web.rs`](../../crates/rimz/src/cli/remote/web.rs) | Web prep, the local auth relay, and tunnel supervision. |
| [`cli/remote/link_stats.rs`](../../crates/rimz/src/cli/remote/link_stats.rs) | The remote-side `rimz remote link-stats ingest` service and listener sampling. |
| [`cli/remote/tty.rs`](../../crates/rimz/src/cli/remote/tty.rs) | `TtyGuard`: local termios snapshot, restore, reply fence, and emulator reset. |
| [`cli/remote/setup.rs`](../../crates/rimz/src/cli/remote/setup.rs), [`cli/remote/list.rs`](../../crates/rimz/src/cli/remote/list.rs) | Install and alias listing handlers. |

Five collaborators sit outside the module and run on the remote host or in the sidebar:

- [`cli/room/attach_exec.rs`](../../crates/rimz/src/cli/room/attach_exec.rs) reads the launch variables when the remote `rimz` launches its mux client: it drives the predecessor reap, writes the attach mark and scroll bracket, and runs the session-loss watchdog (`RemoteRoomWatchdog`).
- [`mux/zellij/reap.rs`](../../crates/rimz/src/mux/zellij/reap.rs) retires an orphaned predecessor client.
- [`cli/room/start_notice.rs`](../../crates/rimz/src/cli/room/start_notice.rs) runs the version-skew gate at room entry.
- [`sidebar/enrich.rs`](../../crates/rimz/src/sidebar/enrich.rs) folds the link sidecar onto the snapshot, and [`sidebar/notify.rs`](../../crates/rimz/src/sidebar/notify.rs) bounds health episodes.
- [`sidebar_pane/render/chrome.rs`](../../crates/rimz/src/sidebar_pane/render/chrome.rs) paints the footer link badge.

## Targets and aliases

A remote target is `[user@]host:<session-or-path>`, a RimZ grammar spelled like `scp`. The suffix after the colon decides which remote command RimZ compiles, and an ambiguous suffix is resolved on the remote host, never against the local filesystem:

| Suffix shape | `RemoteSpec` | Resolution | Remote command |
| --- | --- | --- | --- |
| `session:<name>` | `Session` | Explicit session, even when a directory has the same name. | `rimz attach --attach -- <name>` |
| Contains `/`, or starts with `~` | `Path` | Explicit path; a missing directory fails. | `rimz start --attach -- <dir>` |
| Anything else, including `.agents` and bare names | `Auto` | An existing directory under remote `HOME` wins; otherwise session attach. | `rimz start --attach -- <dir>` or `rimz attach --attach -- <name>` |

Every relative path is anchored to remote `HOME`, independent of the SSH startup directory. Terminal attach, web prep, and link probes compile the same `RemoteTarget::exec_snippet`, so a directory target's probe selects the workspace instead of treating its suffix as a session name.

An `Auto` target is resolved again at each launch, reconnect, and probe start. Creating or removing a same-named directory between those moments can make an attach and its link probe select different workspaces; an explicit `session:` or path target pins the choice.

`RemoteTarget::parse` returns a `RemoteTargetError` whose message carries the expected shape and a fix. Three cases matter before touching it:

- A bracketed IPv6 host may open the string or follow the `@` that ends the user prefix; an `@[` after the first colon belongs to the suffix.
- `~` and `~/…` normalize to `$HOME`, which `quote_remote_path` keeps outside the single quotes so the remote shell expands it.
- `~user` is rejected, because the single-quoted snippet would carry it literally into a junk path.

`SshDestination::parse` handles the colon-less `[user@]host` form that `rimz remote setup` accepts.

Aliases persist per machine at `~/.rimz/remote.toml`, one `[[remote]]` table per entry, sorted by name and written with temp-file-plus-rename.

| Field | Default | Effect |
| --- | --- | --- |
| `name` | required | 1 to 64 ASCII alphanumerics, `-`, or `_`, never leading `-`; checked on every add, update, and load. |
| `target` | required | Parsed through `RemoteTarget::parse` on add and update. Load checks names only, so a hand-edited bad target fails when `rimz remote connect` resolves it. |
| `reconnect` | `true` | `false` hands the link to a single `ssh` run with no supervisor. |
| `no_resume` | `false` | Passes `--no-resume` to the remote room. |
| `mux` | unset | Pins `--mux` for this alias. |
| `auto_forward` | `true` | `false` disables listener forwarding for this alias. |

`resolve_connect` in [`cli/remote.rs`](../../crates/rimz/src/cli/remote.rs) merges the alias with the invocation. An input containing `:` is a raw target and skips aliases. Boolean opt-outs compose (`alias.reconnect && !no_reconnect`), and `--mux` on the command line wins over the alias.

## The command RimZ runs on the host

Every remote invocation ships one shell word to the remote login shell. The terminal attach (`guarded_snippet`) and the web prep one-shot (`web_snippet`) both build it through `remote_exec_snippet`, in a fixed order:

1. Repair `PATH`, because a non-login shell often lacks `~/.cargo/bin`, `~/.local/bin`, `/opt/homebrew/bin`, and `/usr/local/bin`.
2. Run `command -v rimz`, or print the install fix and exit `127`. The supervisor maps that exit to `rimz remote setup <original-input>`.
3. For a `Path` target, run `test -d`, or name the missing path and exit `67`. The guard lives in the snippet so one-shot connections, the interactive fallback, and any remote RimZ version refuse a missing explicit path before room birth.
4. Export the environment the remote room reads.
5. `exec` into `rimz`, choosing directory or session first for an `Auto` target, so no shell survives between SSH and the room.

The probe stream (`probe_stream_spec` in `link.rs`) repairs `PATH` and execs `rimz remote link-stats ingest` with the same target resolution, but skips the missing-binary and path guards: a probe that cannot start is best-effort and must not print into the user's session.

The exported environment is the whole channel from client to host. The web prep snippet exports a subset, marked in the last column.

| Variable | Carries | Web prep |
| --- | --- | --- |
| `RIMZ_REMOTE_LINEAGE` | A stable 16-hex identity for this device plus this room, so a replacement attach can retire its own orphan. | no |
| `RIMZ_REMOTE_SUPERVISED` | Set when the local reconnect supervisor will interpret the session-loss sentinel. | no |
| `RIMZ_REMOTE_CLIENT_VERSION` | The local binary's semantic version, for the skew gate. | yes |
| `RIMZ_REMOTE_FORCE_VERSION` | Set by `--force-version`, downgrading a minor refusal to a warning. | yes |
| `RIMZ_REMOTE_RECONNECT` | Set on retry attempts only, selecting the room's unattended posture. | no |
| `RIMZ_ATTACH_MARK` | Set when a colored connection panel holds the alternate screen, asking the remote RimZ to draw a green check before mux launch. | no |
| `RIMZ_OUTER_SCROLL_BRACKET` | Set on a one-shot attach whose local launcher already owns the alternate-scroll bracket. | no |
| `RIMZ_CLIENT_SIZE` | Locally probed `<cols>x<rows>`, seeding the sidebar when the remote birth has no pty. | yes |
| `COLORTERM` | `truecolor` when the local terminal advertises 24-bit color, which SSH does not forward. Web prep always exports it. | yes |
| `TERM` | Set by the `TermPlan` below. Web prep always exports `xterm-256color`. | yes |

`TermPlan` decides how the remote session resolves the local terminal, because `ssh -t` carries `$TERM` across and a remote mux client aborts when its terminfo entry is missing.

| Plan | When | Snippet |
| --- | --- | --- |
| `Keep` | `$TERM` is unset, empty, or one of the 14 names in `term_needs_terminfo_copy`: `xterm`, `xterm-color`, `xterm-16color`, `xterm-256color`, `screen`, `screen-256color`, `tmux`, `tmux-256color`, `vt100`, `vt102`, `vt220`, `ansi`, `linux`, `dumb`. | Nothing. |
| `Copy` | Any other name, and local `infocmp -x` produced a source. | Export `xterm-256color`, pipe the source through remote `tic -x -`, then export the real name on success. |
| `Downgrade` | Any other name, and no usable `infocmp` source. | Export `xterm-256color`. |

`RIMZ_REMOTE_RECONNECT` matters when debugging a reconnect. `start_attended` in `cli/room/mod.rs` treats a start carrying it as unattended even though `ssh -t` gave it a terminal: hook installation prints its non-interactive notice, the project trust prompt is not offered, the first-run wizard is skipped, and the resume prompt runs silently. A reattach to a live room skips hook detection whatever the posture.

### Version skew

The gate runs on the host, in `report_version_mismatch_notices`, comparing `RIMZ_REMOTE_CLIENT_VERSION` against the host binary. `version::classify` compares the first differing numeric component of the `major.minor.patch` core, ignoring prerelease and build suffixes. A client that exports no version produces no notice.

| Skew | Behavior | Exit |
| --- | --- | --- |
| `Match` | Silent. | none |
| `Patch` | Warn and proceed. | none |
| `Unparseable` | Warn and proceed. | none |
| `Minor` | Refuse, unless `--force-version` downgrades it to a warning. | `65` (`REMOTE_VERSION_SKEW_EXIT`) |
| `Major` | Refuse; `--force-version` does not apply. | `66` (`REMOTE_VERSION_INCOMPATIBLE_EXIT`) |

Both exits reach the local side as fatal verdicts, and `fatal_session_message` names the upgrade fix.

### Retiring the predecessor client

Zellij 0.44 can reuse a client id before the queued removal of the old client has drained, which leaves a reconnect painting into a dead client slot. Before an executable Zellij attach, the remote `rimz` reaps the clients of its own lineage (`reap_remote_zellij_predecessors`).

`remote_lineage` derives the identity by SHA-256 hashing a domain tag plus the length-prefixed local hostname, local user, remote host, spec kind, and spec value, and hex-encoding the first eight bytes. The same device and room always produce the same value, and a different device produces a different one. Attachments from two machines therefore coexist, while a second attach from one machine moves the room.

`reap_lineage_clients` selects `zellij attach --create <session>` processes carrying that lineage, excluding its own process ancestry, and sends `SIGTERM`, escalating to `SIGKILL` after a 500ms grace. It then polls `zellij --session <name> action list-clients` until the human-client count falls. That successful reply is the ordering fence: Zellij's screen worker has processed the predecessor removal before the replacement attach registers. The whole sequence is bounded at two seconds.

The reap is best-effort, and it is skipped in four cases: tmux, an attach invoked from inside the target multiplexer (so an explicit nested `--attach` cannot retire the outer client displaying that pane), an empty or missing `RIMZ_REMOTE_LINEAGE`, and platforms without a readable process environment. When the room has a workspace id, each reap records a `client_reaped` diagnostic with killed pids, pre and post client counts, and settlement or timeout. A reap that does not settle prints `Zellij predecessor cleanup did not settle; attaching anyway` and attaches.

## The connect loop

`supervise_remote` runs the same shape for the first connection and for every recovery: prove the transport out of sight, then hand the proven connection to a visible attach.

```text
supervise_remote
     │
     ▼
 wait_for_master ........... runs behind the connection panel
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
     │                       Verdict::Fatal     → bail with the mapped message
     │                       Verdict::Retry     → back to wait_for_master
     │                       Verdict::Reattach  → new ssh -t on the live master
     │
     └── NeedsInteractive ►  one foreground ssh -t, initial connect only
                             (prompts are usable; a failure there is fatal)
```

### Proving the transport

The background master proves transport and authentication before anything paints, so the panel never shows an all-healthy frame it cannot back. RimZ launches `SshAttachPlan::master` (`ssh -M -N -o BatchMode=yes`, stderr piped) on the control socket `<runtime home>/rimz/link/link-<pid>.sock`, then runs `ssh -S <socket> -O check` every 200ms with a 500ms timeout. On the initial connection, a `Path` target then runs `test -d` over the same socket; exit `1` is the one conclusive missing-path result, and any other exit defers to the normal attach. The visible `ssh -t` reuses the socket.

`MasterState` in `cli/remote/supervisor.rs` holds three variants, and each tick yields one outcome:

| State | Tick outcome | Condition |
| --- | --- | --- |
| `Idle` | enter `Connecting` | An attempt is due (immediately on the initial connection, then when the pacer allows) and the control directory prepares cleanly. |
| `Connecting` | connected | `-O check` succeeds and the panel's minimum display has elapsed. |
| `Connecting` | enter `Ready` | `-O check` succeeds while the panel still owes display time. |
| `Connecting` | failed | The child exits, or `master_deadline` (30 seconds) passes. |
| `Ready` | connected | The panel's release time arrives. |
| `Ready` | failed | The child exits. |

The hidden master pins `ControlPersist=no`, `ConnectionAttempts=1`, and `ClearAllForwardings=yes`, and the attach and web prep clients pin `ControlMaster=auto` with `ControlPersist=no`. Inherited SSH configuration therefore cannot background, multiply, or add forwarding side effects to a supervised connection. Identities, proxies, host-key policy, and authentication stay user-owned. Every attach and master sets `Compression=yes` and `ServerAliveInterval=5`, `ServerAliveCountMax=3`. The master's `ConnectTimeout` is `master_connect_timeout` (5 seconds); the visible attach keeps `ConnectTimeout=10`.

### The interactive fallback

A batch-mode master cannot answer a password, two-factor, or host-key prompt. On the initial connection, a master child that exits with stderr that `transport_failure` does not recognize releases the panel for exactly one foreground interactive attach; a master that times out never does. A failure in that attach is fatal, so RimZ never loops a password prompt. Recovery stays batch-only and retries until Ctrl-C. The fallback inherits stderr so authentication prompts, banners, and host-key warnings remain usable.

### The handoff

The handoff keeps the alternate screen and click and wheel mouse reporting alive from panel to multiplexer. When the master is confirmed, the panel turns its checkpoints green and animates the yellow Multiplexer `attaching…` row for the rest of the minimum display. The final frame freezes that row with an arrow, parks the cursor on its symbol cell, and releases raw input without leaving the alternate screen or dropping mouse capture. The attached multiplexer paints directly over that frame, and residual scroll momentum stays a mouse event instead of becoming an arrow key.

The session end decides how the screen is restored. A non-fatal end restores the main screen and mouse mode immediately. A fatal end keeps the remote output visible, appends the failure and a keypress hint, and restores after a key. Ctrl-C and the interactive fallback return to intact pre-panel scrollback.

Two variables coordinate the handoff with the remote RimZ in `cli/room/attach_exec.rs`:

- The supervisor requests `RIMZ_ATTACH_MARK` only when a panel held the alternate screen and color is enabled. Immediately before the mux client launches, the remote writes a green check at the parked cursor; pty ordering puts it ahead of the mux's first paint. A remote that does not read the variable leaves the arrow until the mux renders.
- A one-shot `--no-reconnect` attach owns the alternate-scroll (mode 1007) save, disable, and restore bracket in the local process and exports `RIMZ_OUTER_SCROLL_BRACKET`, so the remote does not nest a second bracket. Under reconnect supervision the local side sets no bracket, and the remote RimZ owns it.

An attach over a confirmed master pipes and drains SSH stderr so control-client diagnostics cannot paint over the frame. `attach_error_summary` filters direct and shared-connection close notices and maps `mux_client` and control-socket errors to `SSH control connection dropped`; a transport exit with no remaining diagnostic gets the same summary. That cause carries into recovery or the fatal message, and a clean exit prints `rimz: detached from <host>`.

### Classifying the end

`ReconnectState::settle` turns a finished `ssh` into a `Verdict`, checking rows in this order. The Ctrl-C row is checked first by `attach_interrupted`, before `settle` runs.

| Exit | Evidence | Verdict |
| --- | --- | --- |
| `255` with OpenSSH's `Killed by signal 2.` | any | Explicit Ctrl-C: return to the caller without retrying. |
| `0` | any | `CleanExit`: return to the caller. |
| remote RimZ precondition sentinel (`65`, `66`, `67`, `127`) | any | `Fatal`: surface the version, path, or install fix. |
| session-loss sentinel (`68`) | any | `CleanExit`: report `remote room on <host> ended` and return. |
| `255` | established | `Retry`: enter background recovery. |
| other nonzero | established, master ended | `Retry`: SSH liveness outranks the attach status. |
| other nonzero | master alive, attach lived past gatetime | `Reattach`: replace only the multiplexer client over the existing SSH connection. |
| other nonzero | no settled evidence | `Fatal`: an immediate remote room or authentication failure. |
| signal death | any | `Fatal`: something killed `ssh` deliberately. |

A session counts as established once its link probe receives the first ack, once a master (initial or recovery) is confirmed, or once the foreground attach lives past the gatetime (30 seconds by default). Living past the gatetime is also the evidence that the multiplexer attach ran instead of failing at launch. `ReconnectState` folds establishment and consecutive failures across sessions, and `settle_zombie_kill` records an intentional kill without classifying its signal exit as fatal.

### The session-loss watchdog

Both tmux and Zellij can exit `0` when their attached session is destroyed, exactly as for a detach, so the exit code alone cannot tell the two apart. When `RIMZ_REMOTE_SUPERVISED` is set, the remote RimZ runs `RemoteRoomWatchdog` beside the mux client:

1. Every two seconds (`REMOTE_ROOM_WATCHDOG_INTERVAL`) it queries `session_liveness`. A `Live` answer arms the watchdog.
2. Once armed, a missing or exited session kills and reaps the attach client and exits `68`.
3. After the mux client exits on its own, it repeats the check to cover the race between the last poll and the exit, and exits `68` only if the watchdog was armed.

A failed liveness query is unknown and preserves the client's status, so uncertain evidence never overrides an intentional detach. One-shot `--no-reconnect` attaches do not export the variable and keep the mux client's status.

## Terminal hygiene

`TtyGuard` in `cli/remote/tty.rs` keeps the local terminal usable across every session boundary. At connect it snapshots termios, repairing a leftover raw tty first (`termios_damaged`, `sanitize_flags`). After each SSH session it restores the snapshot.

Before every replacement attach, `settle_terminal_replies` fences replies addressed to the dead SSH generation. It puts the tty in raw mode, writes a DSR status query (`ESC[5n`) to stderr, and discards input one byte at a time through the terminal's status answer (`ESC[0n`). Terminals answer queries in order, so every DA1, DA2, or XTVERSION reply for the dead generation precedes the fence reply, and type-ahead after it reaches the replacement byte-exact. Without the fence, tmux passes a duplicate reply to the focused pane as keystrokes. DSR is the fence because the dead client's outstanding DA1 reply would be indistinguishable from a DA1 fence reply.

The fence has three bounds:

- A terminal that ignores `ESC[5n` costs the full two-second cap (`SETTLE_MAX`) on every replacement attach, after which RimZ flushes input, losing type-ahead from that interval. A reply that arrives after the cap can still reach the replacement client; the cap bounds RimZ's wait, and this residual is accepted.
- When stderr is not a terminal, the query cannot be written, and RimZ drains until 250ms of quiet (`SETTLE_QUIET`) and flushes. Ctrl-C ends this quiet-window drain at once; during a fence it is discarded like any other byte.
- Keystrokes typed during an outage are discarded, because no session owns them.

Exit paths and the initial attach never drain, preserving type-ahead for the shell that regains the terminal. Every session end writes `EMULATOR_RESET` after releasing or holding its handoff screen, so mouse, focus, paste, and alternate-screen restoration never depends on the child's cleanup. While a full-screen guard owns the terminal, the tracing writer buffers warnings instead of writing through the frame; the panel renders their cleaned tail, and restore replays the original bytes to stderr. The live `backend::tmux::remote_reconnect` integration suite exercises the fence against a real query-producing tmux client.

## Reconnect pacing and reachability

`AttemptPacer` decides when the next master attempt may start. It fuses the configured checkpoints under one rule: any positive result means the network is up, the network is down only after every configured probe reports down, and an unknown first result stays optimistic.

| Network | Pacing |
| --- | --- |
| Up | `reachable_retry` (2s) until the outage is `flat_window` (3 minutes) old, then doubling once per minute to `backoff_cap` (30s). |
| Down | `1s, 2s, 3s, …, 10s`, then `20s`, then `30s` for every later attempt. |

Two edges reset both ladders and schedule an immediate attempt: a down-to-up probe transition, and a change in the local route fingerprint. The fingerprint is the source address the kernel selects for a UDP route lookup toward `1.1.1.1:443`; the lookup sends no packet.

Endpoint discovery runs once at supervisor startup. `ssh -G -- <destination>` reports the effective `hostname` and `port` after user config applies, and `parse_dial_plan` turns them into a `DialPlan`. A configured `ProxyJump` or `ProxyCommand` yields no plan, because a direct dial would not test the path SSH uses. A failed query or unparseable output also yields none and leaves only timed pacing. DNS resolution stays per-dial, so a network change can supply a fresh address.

During initial connect and during an outage, RimZ dials the endpoint every second with a two-second TCP timeout (`DIAL_TIMEOUT`) and fetches the internet checkpoint with a one-second HTTP timeout. Before each dial, a packet-free route lookup finds the owning local interface. `is_tun_interface` treats a name starting with `tun`, `utun`, `wg`, or `tailscale`, and any point-to-point interface, as reachable without a dial, because a direct probe can blackhole over such a route while SSH succeeds through it.

These workers drive presentation and pacing and never decide truth. The background master remains the end-to-end proof.

| `ReconnectPolicy` field | Default | Meaning |
| --- | --- | --- |
| `gatetime` | `30s` | Lifetime that makes an unconfirmed session count as established. |
| `reachable_retry` | `2s` | Flat retry delay while the network is up. |
| `flat_window` | `3min` | Outage age at which flat pacing gives way to doubling. |
| `backoff_cap` | `30s` | Ceiling for both ladders. |
| `master_connect_timeout` | `5s` | Per-attempt TCP connect and SSH banner budget. |
| `master_deadline` | `30s` | Total lifetime of one background master attempt. |

## The connection panel

An interactive terminal gets a full-screen panel for the initial connection and for each outage, shown after a 500ms grace (`RECOVERY_GRACE`). `panel_allowed` falls back to plain stderr lines when stdout is not a terminal, `RIMZ_NO_PROGRESS` is `1` or `true`, `TERM` is `dumb`, or an agent owns the terminal.

The panel has four rows, one per pipeline stage. Network rows hold their last settled result across attempts, including a regression to unreachable, and the last row stays present throughout so the centered layout is stable at handoff.

| Row | Checkpoint |
| --- | --- |
| Internet | `GET http://cp.cloudflare.com/generate_204` returning status `204`. |
| Server | The effective SSH endpoint from `ssh -G`. Omitted for proxy targets. |
| SSH session | The next master attempt. |
| Multiplexer (`Web tunnel` under `--web`) | Waiting until the master confirms, then `attaching…` (`opening…` for the web tunnel). |

`StageStatus::Suspect` is the one non-obvious status. Once an SSH attempt has failed, a Server row that still answers TCP reads yellow with `answers TCP · SSH failing`, instead of green beside a failing connection. A TUN route reads `via TUN <interface> · TCP check skipped`, and becomes `via TUN <interface> · SSH failing` under the same condition.

Exactly one row animates: the Internet row while waiting for the network, the SSH session row while a master connects, and the last row after confirmation. The active phase, countdown, and final OpenSSH stderr line fold into that row, while a dim header carries the attempt number, elapsed time, and `Ctrl-C stops`. Initial wording says `Connecting`, recovery says `Connection lost`. The block is centered with left-aligned rows and fixed label columns.

Once shown, the panel stays at least 1.5s (`RECOVERY_MIN_DISPLAY`), so a fast reconnect spends the remaining hold on the attaching animation instead of flashing. A connection that lands inside the grace never shows a panel, mark, or handoff frame. A fatal failure before attach replaces the panel with the error, its fix, and up to five captured warning lines, then holds until a key. A fatal failure after handoff appends its error and keypress hint without repainting. Restore returns to the main screen before the CLI prints the error again, so it lands in scrollback.

Ctrl-C is polled as a terminal event in raw mode while the panel owns the terminal: it kills a pending master, restores the terminal, and stops the supervisor. Once SSH owns the terminal, OpenSSH reports a remote command interrupted by Ctrl-C as status 255 plus `Killed by signal 2.`, which `attach_interrupted` recognizes before transport classification.

## Link health

When reconnect is enabled, the supervisor runs a long-lived probe stream over the same ControlMaster connection as the attach, so its measurements describe the user's session path instead of an ICMP round trip:

```text
ssh -S <control-sock> -o BatchMode=yes -- <host> '<PATH repair>; exec rimz remote link-stats ingest --session <name>|--dir <path>'
```

The local side writes one JSON line every two seconds (`LINK_PROBE_INTERVAL`), plus an extra line whenever the displayed RTT changes. The remote `ingest` replies with one JSON ack per line and republishes a sidecar file. A stream that does not start on a confirmed master first waits for `control_check_spec` to succeed: `ssh -S <path>` without `-O check` would open a fresh TCP connection when the socket is absent and measure a link nobody uses.

### Protocol

All three shapes carry the schema version `rimz.link.v1`. `LinkProbe` carries `seq`, `sent_at_ms`, and the stats settled before this line, so the remote file always holds a complete window. `LinkAck` carries `seq` and an optional `ports` array. `LinkStatsFile` carries the remote `received_at_ms`, the SSH client identity (`SSH_CONNECTION`), and the latest stats.

The `ports` field is optional so both skew directions keep working: a remote that never sends it pairs with a local that reads it, and a local that does not read it ignores it. A probe whose version the remote rejects makes `ingest` exit `2`. The local probe loop treats exit `2` or `127` as terminal and stops probing without touching the room.

### Measurement

`ProbeWindow` keeps the latest 30 settled outcomes (`LINK_WINDOW`). A pending probe expires into a miss after the two-second timeout, and a late ack for an already-missed probe is ignored, so loss is the miss percentage over settled probes.

RTT is an EWMA whose smoothing factor adapts. A sample within 8% of the current value uses `alpha = 0.15`, and the factor ramps to `0.60` as the relative deviation approaches 50%, so a real path change moves the number quickly while jitter does not. The displayed value holds until the smoothed value moves at least 8ms, which keeps the badge steady. Each new probe stream discards its first ack sample, because the stream execs a fresh remote `ingest` whose spawn cost would read as latency; the displayed value stays put until real samples arrive.

`LinkMonitor` wraps the window and emits three `LinkEvent`s: `FirstAck` the first time any ack lands, `Blackout(duration)` when no ack has arrived for eight seconds (`LINK_BLACKOUT_AFTER`, latched so one outage produces one event), and `Recovered` on the next ack after a latched blackout.

### The session link machine

`SessionLinkState` reduces gatetime, link events, and session boundaries into actions the supervisor renders. Outage state spans reconnects; session-local state resets through `begin_session`.

| Input | Effect | Action |
| --- | --- | --- |
| `begin_session` | Clears session-local state, remembers whether an outage is open. | none |
| Elapsed reaches gatetime | Marks established. | `Restore`, when the session began during an open outage |
| `FirstAck` | Marks established and confirmed, clears zombie watch. | `Restore`, when an outage was open |
| `Blackout(d)` | Arms zombie watch, opens the outage. | `NotifyBlackout(d)`, once per outage |
| `Recovered` | Clears zombie watch. | `Restore` |
| `transport_lost` | Opens the outage. | `NotifyTransportLoss`, once per outage |
| Zombie watch armed and established | Schedules the next check. | `VerifyZombie` |
| `finish` | Settles an exited child. | none |

`finish` emits nothing on purpose: child exit outranks link presentation, so a session that ends while a blackout notification is queued reports its exit instead of a stall.

### The zombie guard

A suspended laptop or a NAT rebind can leave an SSH child holding the tty on a transport that will never carry another byte. OpenSSH keepalives (`ServerAliveInterval=5`, `ServerAliveCountMax=3`) reach exit `255` in roughly fifteen seconds for a hard loss, but a zombie transport can outlive them.

`verify_zombie` replaces the session only when three conditions hold: the session is established, its probe stayed silent past the blackout threshold, and a fresh endpoint dial succeeds (a TUN route satisfies the dial without one). Failing any one, RimZ waits, because a slow link and a dead link look identical from the blackout alone. The child is then killed with `SIGKILL` so the replacement can claim the terminal, and RimZ prints `link to <host> confirmed dead`. The guard needs a dial plan and a nonzero dial cadence, so proxied configurations, `--no-reconnect`, disabled probes, and probe version skew rely on OpenSSH keepalive death alone.

### Publication and freshness

`ingest` writes `<runtime>/<workspace>/link-stats.json` with temp-file-plus-rename cache semantics, and the sidebar reads it on every enrichment fold. When the probe stream ends, `ingest` removes the sidecar if its `client` still matches this connection's `SSH_CONNECTION`; age expiry covers hard drops where no shutdown ran. Local rooms never have the file, so their footer shows no badge.

| Age | Freshness | Footer |
| --- | --- | --- |
| Up to 10s | Fresh | The measured badge, or `⇄ remote …` while RTT is still warming. |
| 10s to 120s | Stale | `⇄ remote ?` in the muted tone. |
| Past 120s | Expired | No badge. |

### Two health scales

The module keeps a stepped scale for alerting and a continuous one for the badge, and mixing them up is an easy bug.

`LinkTier` is the stepped scale. The type lives in `ids` beside the other shared classification enums, and the snapshot's `SidebarLinkHealth` record embeds it. `remote::link::link_tier` owns the thresholds and takes the worse of the two axes:

| Axis | Good | Degraded | Bad |
| --- | --- | --- | --- |
| RTT | up to 150ms | 151 to 400ms | above 400ms |
| Loss | 0% | 1% to 10% | above 10% |

`link_badge_heat` is the continuous scale: latency maps linearly over `100..=400ms`, loss over `0..=30%`, and the badge takes the worse axis. The renderer turns that `0.0..=1.0` value into a tone through `Theme::heat_tone` ([theme.md](./theme.md)), which keeps the link module theme-free, and adds bold at the red end so a critical link stays loud without color. A warming badge with no RTT sample returns `None` and paints the neutral resting tone. The badge appends `{n}%` only when loss exceeds 10%.

The scales disagree on purpose: low nonzero loss can open an alert episode while the badge still reads close to green.

### Alerts

Alerts split across two delivery paths, by who can still reach the user.

The local supervisor owns outage alerts, because a dead link cannot rely on the remote-rendered sidebar. Confirmed link-lost and link-restored edges emit terminal-local OSC and BEL plus notification handlers. A probe blackout emits the terminal-local signals only, because it is a local stall and not a confirmed drop.

The remote sidebar owns degraded-but-alive links. They surface through the footer badge alone and raise no bell or handler ([sidebar/notifications.md](./sidebar/notifications.md#remote-link-alerts)). The sidebar still bounds a health episode for the record: ten seconds of fresh degraded or bad stats opens one, thirty seconds of fresh good stats closes it, stale stats pause both clocks, and each edge writes a `link_alert` diagnostic carrying the tier, RTT, miss percentage, episode start, and recovery duration.

## Port auto-forwarding

A dev server started in a room pane after attach becomes reachable on the same local port, with no second SSH command. Discovery rides the probe ack, so the feature needs the supervised link and is off under `--no-reconnect` and `--web`.

On the host, `ingest` samples `/proc/net/tcp` and `/proc/net/tcp6` at most every five seconds and attaches the latest `ports` array to each ack. `candidate_ports` keeps a listener in state `0A`, owned by the room user's uid, on port 1024 or above, and bound to a loopback or wildcard address. It reports at most 32 ports, sorted and deduplicated. A host without those procfs files sends no `ports` field, and the connection is otherwise unaffected.

Locally, `PortSync` diffs the reports:

- The first report of the connection becomes a permanent baseline and opens nothing, so services running at attach are never forwarded.
- A port absent from the baseline is opened, up to 16 active forwards.
- A port missing from three consecutive reports is closed.
- An open that fails, because the local port is taken or `ssh -O forward` refuses, is parked until that listener disappears from a report and later returns.

Before asking the master to forward, `apply_port_actions` binds `127.0.0.1:<port>` itself and releases it, which turns a busy local port into a park instead of a stream of refusals. A bind-failure park is reported to the attached user through the local terminal notification channel, respecting notification preferences without running link-event handlers. The notice names the port and tells the user to free it, then stop and restart the server on the host to retry. The forward is `ssh -O forward -L 127.0.0.1:<port>:localhost:<port>` on the live master with a two-second timeout, and `ssh -O cancel` with the same argument closes it, so the local side never listens on a public address.

The baseline and active set live for the whole `rimz remote connect`, across probe-stream and transport replacements. A replacement master reopens the active set before reports resume, and the master's exit tears every forward down at detach. Restarting `rimz remote connect` takes a new baseline.

## Web tunnels

`rimz remote connect <target> --web` opens the remote room in a local browser and stays in the foreground supervising the tunnel. The prep payload, the local auth relay, its port selection, and the security boundary belong to [web.md](./web.md#remote-rooms); this section covers only how the tunnel rides the SSH supervision above.

`run_supervised_web` creates a `LinkSupervisor`, which establishes the initial master through `wait_for_master` with a Web tunnel row in place of Multiplexer. Each round then runs three steps over the confirmed master:

1. Run remote `rimz web open --print --json` as a non-PTY one-shot with stderr inherited. A prep exit of `255` is a transport failure and enters recovery; `127`, `65`, `66`, and `67` get the same fatal messages as terminal attach; any other failure aborts.
2. Reserve a fresh ephemeral local port and install `-L 127.0.0.1:<ephemeral>:127.0.0.1:<tunnel_port>` through `ssh -O forward` on the master. The first round binds the user-facing relay port; later rounds retarget the relay at the new forward.
3. On the first round, print the URL to stdout, open the browser best-effort, and print `rimz: tunnel up` to stderr; later rounds print `rimz: tunnel to <host> restored`. Then wait on the master until Ctrl-C or transport exit.

The confirmed master or the accepting local port marks a round established, so a transport exit `255` after either proof calls `LinkSupervisor::recover`, which re-runs `wait_for_master` behind the panel. Recovery repeats prep so a changed remote port or credential is picked up, and the browser URL stays stable because only the relay's upstream changes. The `-O forward` client exits once the master confirms the listener, so its clean exit is not a detach. The web path runs no probe stream, zombie guard, or port auto-forwarding.

An initial connection that needs interactive authentication runs one round without a master: prep uses a direct connection, and the tunnel is a separate `ssh -N` child (`web_tunnel_spec`). Recovery after it is batch-only. `--no-reconnect` (`run_direct_web`) skips the supervisor and panel entirely and uses the same direct prep and tunnel child; the tunnel pins `ControlMaster=no` and `ControlPath=none` so ambient SSH multiplexing cannot move the forward away from its foreground child, and the command exits when that child ends.

## Bandwidth attribution

`rimz pane bandwidth --secs 5` measures what a room costs its link. It samples the room it runs in and reports each pane's Linux process write-rate, plus the SSH wire-rate when an SSH client is attached. It must run on the Linux host serving the room, as the room's user, and without `sudo`, because privilege escalation resets backend and socket resolution away from the room. The accounting lives with the pane primitives in [`pane/bandwidth.rs`](../../crates/rimz/src/pane/bandwidth.rs), and [`cli/pane/bandwidth.rs`](../../crates/rimz/src/cli/pane/bandwidth.rs) owns sampling and presentation; this page documents it because the `WIRE(ssh)` rows describe the remote transport.

The command resolves the current workspace session, lists panes through the selected backend, pins each pane's root process tree at the start of the window, and reads `/proc/<pid>/io` `wchar` before and after the sleep. For a remote room it also matches the attached mux client's `SSH_CONNECTION` tuple to its socket and reads `ss` TCP_INFO counters (`bytes_acked`, `bytes_received`) over the same window, printed as `WIRE(ssh↑)` for egress to the client and `WIRE(ssh↓)` for ingress.

The two numbers measure different points in the pipeline, which is why the command reports both. Per-pane rows are producer write-rate, including non-pty writes such as transcript files. Between there and the socket, the multiplexer diffs and throttles output to the focused tab, and SSH compresses the payload. `WIRE(ssh)` is what crosses the link, normally far below the per-pane sum.

Three notices replace the report when the host cannot support it: no Linux write-rate counters, panes that resolve to no root process, and unreadable `/proc/<pid>/io` entries. macOS process disk counters stay out on purpose, because they would misrepresent terminal output. `WIRE(ssh)` is omitted for local rooms, for rooms with no attached SSH mux client in the process table, and where `ss` is unavailable.

tmux reports pane root pids natively. Zellij pane pids come from the process table through the same matcher the sidebar metrics path uses, so an active uniquely named foreground command binds while an idle look-alike shell abstains. Because the sampler pins the process tree at the first snapshot, short-lived children born mid-window can escape the sample; a longer window catches persistent high-churn TUIs.

## Test seams

Every clock and probe in the module has an environment override, which keeps the integration tests deterministic.

| Variable | Controls |
| --- | --- |
| `RIMZ_SSH_BIN`, `RIMZ_INFOCMP_BIN` | Binary overrides; `tests/fixtures/ssh-trace` is the standard shim. |
| `RIMZ_REMOTE_GATETIME_MS` | Establishment threshold. |
| `RIMZ_REMOTE_MASTER_CONNECT_MS`, `RIMZ_REMOTE_MASTER_TIMEOUT_MS` | Background master connect and total deadlines. |
| `RIMZ_REMOTE_REACHABLE_RETRY_MS`, `RIMZ_REMOTE_FLAT_WINDOW_MS`, `RIMZ_REMOTE_BACKOFF_CAP_MS` | Retry pacing. |
| `RIMZ_REMOTE_GRACE_MS`, `RIMZ_REMOTE_MIN_DISPLAY_MS` | Panel grace and minimum display. |
| `RIMZ_REMOTE_PROBE_MS`, `RIMZ_REMOTE_PROBE_TIMEOUT_MS`, `RIMZ_REMOTE_BLACKOUT_MS` | Probe cadence, timeout, and blackout threshold. `RIMZ_REMOTE_PROBE_MS=0` disables probing. |
| `RIMZ_REMOTE_DIAL_MS` | Reachability cadence. `0` disables endpoint discovery and all TCP dials. |
| `RIMZ_REMOTE_TUN` | Forces a TUN interface name for route classification. |
| `RIMZ_REMOTE_INTERNET_PROBE` | Replaces the internet checkpoint with an HTTP or HTTPS URL that must return `204`. Empty or `0` disables the row. |
| `RIMZ_PORTS_SWEEP_MS`, `RIMZ_PROC_NET_DIR` | Remote listener sampling cadence and the procfs root. |

## Invariants worth preserving

- Keep `remote/` pure. Parsing, argv, classification, and state machines belong there with unit tests; processes, threads, clocks, and terminal writes belong in `cli/remote/`. The one exception is `remote/web.rs`, whose `bind_local_relay` and `reserve_forward_port` bind loopback listeners to pick free local ports: the relay keeps the listener it bound, and the forward port is probed and released for SSH to bind.
- Prove the transport before painting success. The background master's `-O check` is the proof; reachability dials are presentation and pacing only.
- Keep the child alive if and only if the master is alive. The pinned `ControlPersist=no`, `ConnectionAttempts=1`, and `ClearAllForwardings=yes` options exist so inherited SSH configuration cannot break that.
- Require all three guards before killing a session as a zombie: established, blacked out, and independently reachable.
- Restore the terminal on every exit path: snapshot termios at connect, restore after each session, fence terminal replies before each replacement attach, and write the emulator reset at every session end.
- Keep new ack and sidecar fields optional, so each is invisible to a peer on the other side of a version skew.
- Let the remote host own the room. Put a new remote capability behind a `rimz` subcommand the snippet execs, so the local side keeps reaching the host through the one channel it already supervises.

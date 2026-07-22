# Remote attach and link health

`rimz remote connect` attaches to a room that lives on another host. The local process is an SSH launcher and a link supervisor: it parses the target, compiles an `ssh` command, keeps that connection alive across sleeps and network changes, and measures the link. Everything room-shaped — workspace resolution, session birth, the sidebar, the store, the health gate — runs on the remote host's own `rimz`, and the room paints locally because `ssh -t` carries and provisions the terminal.

Start with the [remote guide](../guide/remote.md) for what a user sees and the [CLI reference](../reference/cli/remote.md) for the command surface. Browser access has its own page ([web.md](./web.md)); the backend contracts behind session attach live in [multiplexers.md](./multiplexers.md).

## The two splits that shape the code

**Local versus remote.** The local side owns the SSH child, reconnect decisions, terminal hygiene, the connection panel, link measurement, and port forwards. The remote side owns the room. Neither side reaches across: the local process never opens the remote store, and the remote room never learns that a supervisor is watching its transport, beyond the environment variables the launch snippet exports.

**Pure versus process.** [`crates/rimz/src/remote/`](../../crates/rimz/src/remote/) is a pure library. It parses targets, builds [`CommandSpec`](../../crates/rimz/src/mux/command.rs) values, classifies exits, and advances state machines from elapsed durations the caller supplies. It spawns nothing and reads no clock. [`crates/rimz/src/cli/remote/`](../../crates/rimz/src/cli/remote/) owns every process, timer, thread, and terminal write. Follow that line when adding behavior: a decision belongs in the library with a unit test, and only its execution belongs in the CLI.

## Module map

| Path | Owns |
| --- | --- |
| [`remote/mod.rs`](../../crates/rimz/src/remote/mod.rs) | Target grammar, the guarded remote shell snippet, `TermPlan`, `SshAttachPlan`, lineage derivation, `ReconnectPolicy`, and the `Verdict` classification. |
| [`remote/aliases.rs`](../../crates/rimz/src/remote/aliases.rs) | Saved aliases in `remote.toml`: schema, validation, and atomic CRUD. |
| [`remote/link.rs`](../../crates/rimz/src/remote/link.rs) | The `rimz.link.v1` probe protocol, `ProbeWindow` and `LinkMonitor` accounting, the `SessionLinkState` machine, health tiers and badge heat, and the ControlMaster argv. |
| [`remote/reachability.rs`](../../crates/rimz/src/remote/reachability.rs) | `ssh -G` endpoint discovery, TUN-route classification, and the `AttemptPacer`. |
| [`remote/recovery.rs`](../../crates/rimz/src/remote/recovery.rs) | `RecoveryPanel` checkpoint state, panel timing, and the internet checkpoint endpoint. |
| [`remote/forward.rs`](../../crates/rimz/src/remote/forward.rs) | Remote listener parsing from procfs and the `PortSync` diff state. |
| [`remote/web.rs`](../../crates/rimz/src/remote/web.rs) | Argv for the web prep one-shot and the tunnel. |
| [`remote/setup.rs`](../../crates/rimz/src/remote/setup.rs) | The `rimz remote setup` installer snippet. |
| [`remote/tty.rs`](../../crates/rimz/src/remote/tty.rs) | Termios damage detection, flag repair, and the emulator reset byte string. |
| [`remote/version.rs`](../../crates/rimz/src/remote/version.rs) | Client/host version-skew classification. |
| [`cli/remote.rs`](../../crates/rimz/src/cli/remote.rs) | The clap surface, alias resolution, and the print/exec/supervise fork. |
| [`cli/remote/supervisor.rs`](../../crates/rimz/src/cli/remote/supervisor.rs) | The supervision loop: background masters, foreground attach, probe threads, reachability workers, forwards, notifications. |
| [`cli/remote/outage_ui.rs`](../../crates/rimz/src/cli/remote/outage_ui.rs) | The alternate-screen connection panel and its plain-line fallback. |
| [`cli/remote/web.rs`](../../crates/rimz/src/cli/remote/web.rs) | Web prep, credential display, and tunnel supervision. |
| [`cli/remote/link_stats.rs`](../../crates/rimz/src/cli/remote/link_stats.rs) | The remote-side `rimz remote link-stats ingest` service. |
| [`cli/remote/tty.rs`](../../crates/rimz/src/cli/remote/tty.rs) | The `TtyGuard` that snapshots and restores local termios. |
| [`cli/remote/setup.rs`](../../crates/rimz/src/cli/remote/setup.rs), [`cli/remote/list.rs`](../../crates/rimz/src/cli/remote/list.rs) | Install and alias listing handlers. |

Four collaborators sit outside the module and are easy to miss:

- [`mux/zellij/reap.rs`](../../crates/rimz/src/mux/zellij/reap.rs) retires an orphaned predecessor client on the remote host, driven from [`cli/room/attach_exec.rs`](../../crates/rimz/src/cli/room/attach_exec.rs).
- [`cli/room/start_notice.rs`](../../crates/rimz/src/cli/room/start_notice.rs) runs the version-skew gate, on the host, at room entry.
- [`sidebar/enrich.rs`](../../crates/rimz/src/sidebar/enrich.rs) folds the link sidecar onto the snapshot, and [`sidebar/notify.rs`](../../crates/rimz/src/sidebar/notify.rs) bounds health episodes.
- [`sidebar_pane/render/chrome.rs`](../../crates/rimz/src/sidebar_pane/render/chrome.rs) paints the footer link badge.

## Targets and aliases

A remote target is `[user@]host:<session-or-path>`. What follows the colon decides which remote command RimZ compiles:

| Suffix shape | `RemoteSpec` | Remote command |
| --- | --- | --- |
| Contains `/`, or starts with `~` | `Path` | `rimz start --attach -- <dir>` |
| Anything else | `Session` | `rimz attach --attach -- <name>` |

Parsing lives in `RemoteTarget::parse` and every failure carries the expected shape plus a fix. Three cases are worth knowing before touching that function: a bracketed IPv6 host may open the string or follow the `@` that ends the user prefix, and an `@[` after the first colon belongs to the suffix rather than the host; `~` and `~/…` normalize to `$HOME` so the remote shell expands them past the snippet's quoting; and `~user` is rejected at parse time, because the single-quoted snippet would carry it literally into a junk path.

`SshDestination::parse` handles the colon-less `[user@]host` form that `rimz remote setup` accepts.

Aliases persist per machine at `$XDG_CONFIG_HOME/rimz/remote.toml`, one `[[remote]]` table per entry, sorted by name and written with temp-file-plus-rename.

| Field | Default | Effect |
| --- | --- | --- |
| `name` | required | 1 to 64 ASCII alphanumerics, `-`, or `_`, never leading `-`. |
| `target` | required | Validated through `RemoteTarget::parse` on every add, update, and load. |
| `reconnect` | `true` | `false` hands the link to a single `ssh` run with no supervisor. |
| `no_resume` | `false` | Passes `--no-resume` to the remote room. |
| `mux` | unset | Pins `--mux` for this alias. |
| `auto_forward` | `true` | `false` disables listener forwarding for this alias. |

`resolve_connect` in [`cli/remote.rs`](../../crates/rimz/src/cli/remote.rs) merges the alias with the invocation: an input containing `:` is a raw target and skips aliases entirely, boolean opt-outs compose (`alias.reconnect && !no_reconnect`), and `--mux` on the command line wins over the alias.

## The command RimZ runs on the host

Every remote invocation ships one shell word to the remote login shell. The attach and the web one-shots build theirs through `remote_exec_snippet`, in a fixed order:

1. Repair `PATH`, because a non-login shell often lacks `~/.cargo/bin`, `~/.local/bin`, `/opt/homebrew/bin`, and `/usr/local/bin`.
2. `command -v rimz` or exit `127` after printing the install fix. The supervisor special-cases that sentinel and prints `rimz remote setup <original-input>` instead of its reconnect-policy tail.
3. Export the environment the remote room reads.
4. `exec` into `rimz`, so no shell survives between SSH and the room.

The probe stream is the exception: it repairs `PATH` and execs `rimz remote link-stats ingest` without the missing-binary guard, because a probe that cannot start is best-effort and must not print into the user's session.

The exported environment is the whole channel from client to host:

| Variable | Carries |
| --- | --- |
| `RIMZ_REMOTE_LINEAGE` | A stable 16-hex identity for this device plus this room, so a replacement attach can retire its own orphan. |
| `RIMZ_REMOTE_CLIENT_VERSION` | The local binary's semantic version, for the skew gate. |
| `RIMZ_REMOTE_FORCE_VERSION` | Set by `--force-version`, downgrading the minor refusal to a warning. |
| `RIMZ_REMOTE_RECONNECT` | Set on retry attempts only, selecting the room's unattended posture. |
| `RIMZ_ATTACH_MARK` | Set when a colored connection panel owns the alternate screen, asking a compatible remote RimZ to replace the parked Multiplexer arrow with a green check immediately before mux exec. |
| `RIMZ_CLIENT_SIZE` | Locally probed `<cols>x<rows>`, seeding the sidebar when the remote birth has no pty. |
| `COLORTERM` | `truecolor` when the local terminal advertises 24-bit color, which SSH does not forward. |
| `TERM` | Set by the `TermPlan` below. |

`TermPlan` decides how the remote session resolves the local terminal, because `ssh -t` carries `$TERM` across and a remote mux client aborts when its terminfo entry is missing.

| Plan | When | Snippet |
| --- | --- | --- |
| `Keep` | `$TERM` is in the universal set (`xterm*`, `screen*`, `tmux*`, `vt*`, `ansi`, `linux`, `dumb`) | Nothing. |
| `Copy` | Non-portable name and local `infocmp -x` produced a source | Export `xterm-256color`, pipe the source through remote `tic -x -`, then export the real name on success. |
| `Downgrade` | Non-portable name and no usable `infocmp` source | Export `xterm-256color`. |

The retry posture matters for anyone debugging a reconnect. A remote room start carrying `RIMZ_REMOTE_RECONNECT` behaves as unattended even though `ssh -t` gave it a terminal: hook installation prints its non-interactive notice, project trust stays ungranted, first-run setup waits, and fleet recovery runs silently. A plain reattach to a healthy live room skips hook detection entirely.

### Version skew

The gate runs on the host, in `report_version_mismatch_notices`, comparing `RIMZ_REMOTE_CLIENT_VERSION` against the host binary. `classify` compares the first differing numeric component of the `major.minor.patch` core, ignoring prerelease and build suffixes.

| Skew | Behavior | Exit |
| --- | --- | --- |
| `Match` | Silent | — |
| `Patch` | Warn and proceed | — |
| `Unparseable` | Warn and proceed | — |
| `Minor` | Refuse, unless `--force-version` downgrades it to a warning | `65` (`REMOTE_VERSION_SKEW_EXIT`) |
| `Major` | Refuse; `--force-version` does not apply | `66` (`REMOTE_VERSION_INCOMPATIBLE_EXIT`) |

Both exit codes reach the local side as fatal verdicts with a tailored message. A client predating the version marker sends no evidence and produces no notice; a host predating tiered comparison keeps its older warn-only handling.

### Retiring the predecessor client

Zellij 0.44 can reuse a client id before the queued removal of the old client has drained, which leaves a reconnect painting into a dead client slot. Before an executable Zellij attach, the remote `rimz` reaps its own lineage.

`remote_lineage` derives the identity by hashing a domain tag plus length-prefixed local hostname, local user, remote host, spec kind, and spec value, and taking the first eight bytes as hex. The same device plus the same room always produces the same value; a different device produces a different one, which is why attachments from two machines coexist while a second attach from one machine moves the room.

The reap itself selects `zellij attach --create <session>` processes carrying that lineage, excluding its own process ancestry, sends `SIGTERM`, escalates to `SIGKILL` after a 500ms grace, and then polls `zellij --session <name> action list-clients` until the human-client count falls. That successful reply is the ordering fence: Zellij's screen worker has processed the predecessor removal before the replacement attach can register. The whole sequence is bounded at two seconds.

The seam stays best-effort. An attach invoked from inside the target multiplexer bypasses it, so an explicit nested `--attach` cannot retire the outer client displaying that pane. tmux bypasses it entirely. Platforms without a readable process environment proceed without reaping. Every outcome records a `client_reaped` diagnostic with killed pids, pre and post client counts, and settlement or timeout.

## The connect loop

`supervise_remote` is the heart of the module. It runs the same shape for the first connection and for every recovery: prove the transport out of sight, then hand the proven connection to a visible attach.

```text
supervise_remote
     │
     ▼
 wait_for_master ........... runs behind the connection panel
     │                       reachability workers pace the attempts
     │                       ssh -M -N -o BatchMode=yes, then -O check
     │
     ├── Connected ───────►  start the probe stream, then ssh -t on the same socket
     │                            │
     │                            ▼
     │                       Verdict::CleanExit → return to the caller
     │                       Verdict::Fatal     → bail with the mapped message
     │                       Verdict::Retry     → back to wait_for_master
     │
     └── NeedsInteractive ►  one foreground ssh -t, initial connect only
                             (prompts are usable; a failure there is fatal)
```

**Proving the transport first** is why the master exists. RimZ launches `ssh -M -N -o BatchMode=yes` with piped stderr onto a PID-scoped control socket, then polls `ssh -S <socket> -O check`. A successful check proves transport and authentication before anything paints, so the panel never shows an all-healthy frame it cannot back. The visible `ssh -t` then reuses that socket.

`MasterState` holds three variants and each tick yields one outcome:

| State | Tick outcome | Condition |
| --- | --- | --- |
| `Idle` | enter `Connecting` | The pacer says an attempt is due and the control directory prepares cleanly. |
| `Connecting` | connected | `-O check` succeeds and the panel's minimum display has already elapsed. |
| `Connecting` | enter `Ready` | `-O check` succeeds while the panel still owes display time. |
| `Connecting` | failed | The child exits, or the 30-second `master_deadline` passes. |
| `Ready` | connected | The panel's release time arrives. |
| `Ready` | failed | The child exits. |

RimZ pins `ControlPersist=no`, `ConnectionAttempts=1`, and `ClearAllForwardings=yes` on the hidden master, and `ControlPersist=no` on the attach and web control clients, so inherited SSH configuration cannot background, multiply, or add forwarding side effects to a supervised connection. Identities, proxies, host-key policy, and authentication stay user-owned. Every attach sets `Compression=yes`; an sshd that disallows compression continues uncompressed. Each background master gets a five-second connect and banner budget inside a 30-second total deadline; the visible attach keeps its ten-second connect budget.

**The interactive fallback** exists because a batch-mode master cannot answer a password, two-factor, or host-key prompt. On an *initial* connection, a master failure whose stderr does not match `transport_failure` releases the panel for exactly one foreground interactive attach. A failure there is fatal, so RimZ never loops a password prompt. Recovery stays batch-only and retries until Ctrl-C.

**The handoff** keeps the alternate screen and click/wheel mouse reporting alive across the transition. When the master is confirmed, the still-owned panel turns its checkpoints green and animates the yellow Multiplexer `attaching…` row for the rest of the minimum-display window. The final frame freezes that row with an arrow and parks the cursor on its symbol cell before releasing raw input without leaving the alternate screen or dropping mouse capture, so the attached multiplexer paints directly over that frame and residual scroll momentum remains a mouse event instead of becoming an arrow key. A safety leave restores the main screen and mouse mode after the session ends. Ctrl-C, and the interactive fallback, instead return to intact pre-panel scrollback.

The local supervisor requests `RIMZ_ATTACH_MARK` only when that colored panel held the alternate screen. Immediately before the remote room execs the mux client, a compatible RimZ writes a green check at the parked cursor; pty ordering puts the check ahead of the mux's first paint. An older remote ignores the variable and leaves the arrow in place until the mux renders.

An attach over a confirmed master pipes and drains SSH stderr instead of letting control-client diagnostics paint over the panel. The supervisor filters direct and shared-connection close notices, maps a transport exit with no remaining diagnostic to `SSH control connection dropped`, carries that cause into recovery or a fatal message, and prints `rimz: detached from <host>` for a clean exit. The initial interactive fallback inherits stderr so authentication prompts, banners, and host-key warnings remain usable.

**Classifying the end** of a session is pure:

| Exit | `established` | Verdict |
| --- | --- | --- |
| `0` | any | `CleanExit` — return to the caller |
| `255` | yes | `Retry` — enter background recovery |
| `255` | no | `Fatal` — the link never came up |
| any other code | any | `Fatal` — auth failure, missing remote `rimz`, version refusal, a stuck room |
| signal death | any | `Fatal` — something killed `ssh` deliberately |

A session counts as established once its link probe receives the first ack, once its initial master is confirmed, or once it lives past the gatetime (30 seconds by default). `ReconnectState` folds establishment and consecutive failures across sessions; `settle_zombie_kill` records an intentional kill without classifying its signal exit as fatal.

**Terminal hygiene** wraps every session. `TtyGuard` snapshots local termios at connect, repairs a leftover raw tty at entry, and restores the snapshot after each SSH session. An unclean end also writes the emulator reset string, because `ssh -t` mirrors local tty modes onto the remote pty and a `SIGKILL`ed transport cannot restore terminal-emulator state itself.

## Reconnect pacing and reachability

`AttemptPacer` decides when the next attempt may start. It fuses the configured checkpoints under one rule: any positive result means the network is up, the network is down only after every configured probe reports down, and an unknown first result stays optimistic.

| Network | Pacing |
| --- | --- |
| Up | `2s` for the first three minutes of the outage, then doubling once per minute to the `30s` cap. |
| Down | `1s, 2s, 3s, …, 10s`, then `20s`, then `30s` for every later safety attempt. |

Two edges reset both windows and schedule an immediate attempt: a down-to-up probe transition, and a change in the local route fingerprint. The fingerprint is the source address the kernel selects for a UDP route lookup toward `1.1.1.1:443`, which sends no packet and only reads back the local address the route would use.

Endpoint discovery runs once at supervisor startup. `ssh -G -- <destination>` reports the effective `hostname` and `port` after user config applies; a configured `ProxyJump` or `ProxyCommand` yields no dial plan, because a direct dial would not test the path SSH actually uses. A failed query or unparseable output leaves the timed policy unchanged. DNS resolution stays per-dial, so a network change can supply a fresh address.

During initial connect and during an outage, RimZ dials that endpoint every second with a two-second TCP timeout, and fetches the public internet checkpoint with a one-second HTTP timeout. Before each server dial, a packet-free route lookup identifies the owning local interface. `tun`, `utun`, `wg`, `tailscale`, and any point-to-point interface count as reachable without a TCP dial, because a direct probe can blackhole over such a route even while SSH succeeds through it.

These workers drive presentation and pacing without owning truth. The background master remains the end-to-end proof of transport and authentication.

| Policy field | Default | Meaning |
| --- | --- | --- |
| `gatetime` | `30s` | Lifetime that makes an unconfirmed session count as established. |
| `reachable_retry` | `2s` | Flat retry delay while the endpoint answers. |
| `flat_window` | `3min` | Outage age at which flat pacing gives way to doubling. |
| `backoff_cap` | `30s` | Ceiling for both ladders. |
| `master_connect_timeout` | `5s` | Per-attempt TCP connect and SSH banner budget. |
| `master_deadline` | `30s` | Total lifetime of one background master attempt. |

## The connection panel

An interactive terminal gets a full-screen panel for the initial connection and for each outage, after a `500ms` grace. Redirected output, `RIMZ_NO_PROGRESS`, and an agent-owned terminal fall back to plain stderr transition lines instead.

The panel separates four pipeline stages. Network checkpoints hold their last settled result across attempts, including regressions back to unreachable, and the Multiplexer row remains present throughout so the centered layout stays stable at handoff:

| Row | Checkpoint |
| --- | --- |
| Internet | `GET http://cp.cloudflare.com/generate_204` returning status `204`. |
| Server | The effective SSH endpoint from `ssh -G`. Omitted for proxy targets, whose direct endpoint does not describe the real path. |
| SSH session | The next SSH attempt. |
| Multiplexer | Waiting until the master confirms, then attaching to the remote room. |

`StageStatus::Suspect` is the one non-obvious status. A server that still answers TCP after at least two failed SSH attempts reads yellow with `answers TCP · SSH failing`, rather than a green row beside a failing connection. A TUN route reads `via TUN <interface> · TCP check skipped`, becoming `via TUN <interface> · SSH failing` under the same condition.

Presentation details worth preserving: the panel centers one block with left-aligned rows and fixed label columns; initial-stage wording says `Connecting` while recovery says `Connection lost`; exactly one row animates, the Internet row while waiting for the network, the SSH session row while establishing the master, and the Multiplexer row after confirmation; the active phase, countdown, and final OpenSSH stderr line fold into that row while the dim header carries the attempt, elapsed time, and `Ctrl-C stops`. Once shown, the panel stays for at least `1.5s`, so a fast reconnect uses the remaining hold window for the attaching animation instead of flashing. A connection that lands inside the grace never shows a panel, marker, or handoff frame.

Ctrl-C is polled as a terminal event in raw mode: it kills a pending master, restores the terminal, and stops the supervisor cleanly.

## Link health

When reconnect is enabled, the supervisor runs a long-lived probe stream over the same ControlMaster connection as the attach, so its measurements describe the user's actual session path rather than an ICMP round trip:

```text
ssh -S <control-sock> -o BatchMode=yes -- <host> '<PATH repair>; exec rimz remote link-stats ingest --session <name>|--dir <path>'
```

The local side writes one JSON line every two seconds; the remote `ingest` command replies with one JSON ack per line and republishes a sidecar file. `control_check_spec` gates the stream: `ssh -S <path>` without `-O check` would opportunistically open a fresh TCP connection when the socket is absent, which would measure a link nobody is using.

### Protocol

The schema version is `rimz.link.v1` on all three shapes. `LinkProbe` carries `seq`, `sent_at_ms`, and the best stats settled *before* this line, so the remote file always holds a complete window rather than a half-sent sample. `LinkAck` carries `seq` and an optional `ports` array. `LinkStatsFile` carries the remote `received_at_ms`, the SSH client identity (`SSH_CONNECTION`), and the latest stats.

The optional `ports` field preserves both skew directions: an older remote sends no ports, and an older local ignores the field. A probe whose version the remote rejects makes `ingest` exit `2`; the local probe loop treats exit `2` or `127` as terminal and stops probing without touching the room.

### Measurement

`ProbeWindow` keeps the latest 30 settled outcomes. A pending probe expires into a miss after the two-second timeout, and a late ack for an already-missed probe is ignored, so loss is the miss percentage over settled probes only.

RTT is an EWMA with two behaviors a contributor should expect. The smoothing factor adapts: a sample within 8% of the current value uses `alpha = 0.15`, and the factor ramps to `0.60` as the relative deviation approaches 50%, so a genuine path change moves the number quickly while jitter does not. The *displayed* value additionally holds until the smoothed value moves at least 8ms, which keeps the badge from flickering. Each new probe stream discards its first ack sample, because a stream execs a fresh remote ingest process and would otherwise report cold spawn cost as latency; the previously displayed value stays put until real samples arrive.

`LinkMonitor` wraps that window and emits three events: `FirstAck` the first time any ack lands, `Blackout(duration)` when no ack has arrived for eight seconds (latched, so one outage produces one event), and `Recovered` on the next ack after a latched blackout.

### The session link machine

`SessionLinkState` reduces gatetime, link events, and session boundaries into actions the supervisor renders. Outage state spans reconnects; session-local state resets through `begin_session`.

| Input | Effect | Action |
| --- | --- | --- |
| `begin_session` | Clears session-local state, remembers whether an outage is open | — |
| Elapsed reaches gatetime | Marks established | `Restore`, when a session began during an open outage |
| `FirstAck` | Marks established and confirmed, clears zombie watch | `Restore`, when an outage was open |
| `Blackout(d)` | Arms zombie watch, opens the outage | `NotifyBlackout(d)`, once per outage |
| `Recovered` | Clears zombie watch | `Restore` |
| `transport_lost` | Opens the outage | `NotifyTransportLoss`, once per outage |
| Zombie watch armed and established | Schedules the next check | `VerifyZombie` |
| `finish` | Settles an exited child | none, by design |

`finish` emitting nothing is deliberate: child exit outranks link presentation, so a session that ends while a blackout notification is queued reports its exit rather than a stall.

### The zombie guard

A suspended laptop or a NAT rebind can leave an SSH child holding the tty on a transport that will never carry another byte. OpenSSH keepalives (`ServerAliveInterval=5`, `ServerAliveCountMax=3`) reach exit `255` in roughly fifteen seconds for a hard loss, but a zombie transport can outlive them.

The supervisor replaces the session only when all three conditions hold: the session established, its probe stayed silent past the blackout threshold, and a fresh endpoint reachability check succeeds. A TUN route satisfies that last guard without a TCP dial. Failing any one of them, RimZ waits, because a slow link and a dead link look identical from the blackout alone. Plain-SSH fallback, disabled probes, probe version skew, and proxied configurations keep OpenSSH keepalive death detection instead. A blacked-out SSH child that still owns the tty is killed with `SIGKILL` so the replacement can claim the terminal.

### Publication and freshness

`ingest` writes `<runtime>/<workspace>/link-stats.json` with temp-file-plus-rename cache semantics, and the sidebar reads it on every enrichment fold. When the probe stream ends, its ingest removes the sidecar if the client identity still names it as the last writer; the expiry covers hard drops where no shutdown ran. Local rooms never have the file, so their footer is unchanged.

| Age | Freshness | Footer |
| --- | --- | --- |
| Up to 10s | Fresh | The measured badge, or `⇄ remote …` while the RTT is still warming |
| 10s to 120s | Stale | `⇄ remote ?` in the muted tone |
| Past 120s | Expired | No badge |

### Two health scales

The module keeps a stepped scale for alerting and a continuous one for the badge, and mixing them up is an easy bug.

`LinkTier` is the stepped one, taking the worse of the two axes:

| Axis | Good | Degraded | Bad |
| --- | --- | --- | --- |
| RTT | up to 150ms | 151 to 400ms | above 400ms |
| Loss | 0% | 1% to 10% | above 10% |

`link_badge_heat` is the continuous one: latency maps linearly over `100..=400ms`, loss over `0..=30%`, and the badge takes the worse axis. The renderer turns that `0.0..=1.0` value into a tone through `Theme::heat_tone`, keeping the link module theme-free, and keeps bold weight at the red end so a critical link stays loud without color. A warming badge with no RTT sample yet returns `None` and paints the neutral resting tone. The badge appends `{n}%` only when loss exceeds 10%.

The gap is intentional: low nonzero loss can open an alert episode while the continuous badge still reads close to green.

### Alerts

Alerts split across two delivery paths, by who can still reach the user.

The local supervisor emits terminal-local OSC and BEL plus notification handlers for confirmed link-lost and link-restored edges. Probe blackout emits terminal-local signals only, because it is a local stall rather than a confirmed drop. These stay local because a dead link cannot rely on the remote-rendered sidebar.

Degraded-but-alive edges, while bytes still flow, surface through the footer badge alone and raise no tab bell or notify handler ([sidebar/notifications.md](./sidebar/notifications.md)). The remote sidebar still bounds a health episode for the record: ten seconds of fresh degraded or bad stats opens one, thirty seconds of fresh good stats closes it, stale stats pause both clocks, and each edge writes a `link_alert` diagnostic carrying the tier, RTT, miss percentage, episode start, and recovery duration.

## Port auto-forwarding

A dev server started in a room pane after you attach becomes reachable on the same local port, without a second SSH command. Discovery rides the probe ack, so the feature needs the supervised link and is unavailable under `--no-reconnect` and `--web`.

**On the host**, `ingest` samples `/proc/net/tcp` and `/proc/net/tcp6` at most every five seconds and attaches a `ports` array to the ack. A listener qualifies when it is in state `0A`, owned by the room user's uid, on port 1024 or above, and bound to a loopback or wildcard address. At most 32 ports are reported, sorted and deduplicated. Non-Linux hosts report nothing and the connection is otherwise unaffected.

**Locally**, `PortSync` diffs those reports:

- The first report of the connection becomes a permanent baseline and opens nothing, so pre-existing services are never forwarded.
- A port absent from the baseline is opened, up to 16 active forwards.
- A port missing from three consecutive reports is closed.
- An open that fails, because the local port is taken or `ssh -O forward` refuses, is *parked* until that listener disappears from a report and later returns.

Before asking the master to forward, the supervisor binds `127.0.0.1:<port>` itself and releases it. That check is what turns a busy local port into a park rather than a stream of refusals. The forward itself is `ssh -O forward -L 127.0.0.1:<port>:localhost:<port>` on the live master, and `ssh -O cancel` with the same argument closes it, so the local side never listens on a public address.

The baseline and active set live for the whole `rimz remote connect`, across probe-stream and transport replacements. A replacement master reopens the active set before reports resume, and the master's lifetime tears every forward down at detach. A server that starts within roughly two seconds of connect lands in the baseline; restarting `rimz remote connect` establishes a new one.

## Web tunnels

`rimz remote connect <target> --web` opens the remote room in a local browser and stays in the foreground supervising the tunnel. The attach supervisor first establishes a PID-scoped ControlMaster through the connection panel; prep and the local-forward tunnel then travel as multiplexed connections over that master. Key or agent authentication gives the intended no-prompt flow. An initial connection that needs interactive authentication falls back to the direct flow once; recovery remains batch-only.

The prep command is the fail-fast boundary. RimZ runs remote `rimz web open --print --json` as a non-PTY one-shot with stderr inherited and parses the `rimz.web.v2` payload from stdout. The payload carries the URL, session, port, and Basic-Auth credential in one round trip. A remote without `rimz web`, a host without ttyd, or any remote room error aborts before browser access opens. A prep exit of `127` uses the same missing-binary sentinel as terminal attach, and exits `65` and `66` get the same version-mismatch presentation.

With the payload in hand, RimZ prints the serving machine's Basic-Auth credential, chooses a local port, installs `-L 127.0.0.1:<local>:127.0.0.1:<remote>` through `ssh -O forward` on the confirmed ControlMaster, prints the bare URL after that request succeeds, opens the browser best-effort, and waits on the master until Ctrl-C or transport exit. The control request exits after confirmation while the master owns the listener, so its clean helper exit is not a detach signal. `--web-port` pins the local origin; without it, RimZ hashes the session name into `8300..=8399` and scans forward to the next free port.

The confirmed master or the accepting local port marks a round established, so an SSH transport exit `255` after either proof enters the shared recovery path immediately. The recovery panel carries the Internet, Server, SSH session, and Web tunnel checkpoints while `wait_for_master` paces attempts. After a replacement master connects, RimZ re-runs web prep to revive the remote server and discover a changed remote port and credential, then opens a replacement forward on the same local port. The browser URL stays stable, and a changed credential is printed again.

`--no-reconnect` skips the master and panel. Prep and the tunnel each use a direct SSH connection; the tunnel pins `ControlPath=none` so ambient SSH multiplexing cannot transfer ownership away from its foreground child, and the command exits when that child ends.

The shared daemon, authenticated shim, and credential lifecycle live in [web.md](./web.md).

## Bandwidth attribution

`rimz pane bandwidth --secs 5` measures what a room costs its link. It samples the room it runs in and reports each pane's Linux process write-rate, plus the SSH wire-rate when an SSH client is attached. Run it on the Linux host serving the room, as the room's user, and without `sudo`, because privilege escalation resets backend and socket resolution away from the room. The accounting lives with the pane primitives in [`pane/bandwidth.rs`](../../crates/rimz/src/pane/bandwidth.rs), and [`cli/pane/bandwidth.rs`](../../crates/rimz/src/cli/pane/bandwidth.rs) owns sampling and presentation; the link-cost story stays here because the optional `WIRE(ssh)` rows describe the remote transport.

The command resolves the current workspace session, lists panes through the selected backend, pins each pane's root process tree at the start of the window, and reads `/proc/<pid>/io` `wchar` before and after the sleep. For remote rooms it also matches the attached mux client's `SSH_CONNECTION` tuple to the room's socket and reads `ss` TCP_INFO counters (`bytes_acked`, `bytes_received`) over the same window, printed as `WIRE(ssh↑)` for egress to the client and `WIRE(ssh↓)` for ingress.

The two numbers measure different points in the pipeline, which is the whole reason the command reports both. Per-pane rows are producer write-rate, including non-pty writes such as transcript files. Between there and the socket, the multiplexer diffs and throttles to the focused tab, and SSH compresses the encrypted payload. `WIRE(ssh)` is what actually crosses, normally far below the per-pane sum.

Three notices replace the report when the host cannot support it: hosts without Linux write-rate counters, rooms whose panes resolve to no root process, and rooms whose `/proc/<pid>/io` entries cannot be read. macOS process disk counters stay out deliberately, because they would misrepresent terminal output. `WIRE(ssh)` is omitted for local rooms, for rooms with no attached SSH mux client in the process table, and where `ss` is unavailable.

tmux fills pane root pids natively. Zellij pane pids come from the process table through the same matcher the sidebar metrics path uses, so an active uniquely named foreground command binds while an idle look-alike shell abstains. The sampler pins the process tree at the first snapshot, so short-lived children born mid-window can escape the sample; a longer window catches persistent high-churn TUIs.

## Test seams

Every clock and probe in this module is overridable, which is what keeps the integration tests deterministic instead of timing-dependent.

| Variable | Controls |
| --- | --- |
| `RIMZ_SSH_BIN`, `RIMZ_INFOCMP_BIN` | Binary overrides; `tests/fixtures/ssh-trace` is the standard shim. |
| `RIMZ_REMOTE_GATETIME_MS` | Establishment threshold. |
| `RIMZ_REMOTE_MASTER_CONNECT_MS`, `RIMZ_REMOTE_MASTER_TIMEOUT_MS` | Background master connect and total deadlines. |
| `RIMZ_REMOTE_REACHABLE_RETRY_MS`, `RIMZ_REMOTE_FLAT_WINDOW_MS`, `RIMZ_REMOTE_BACKOFF_CAP_MS` | Retry pacing. |
| `RIMZ_REMOTE_GRACE_MS`, `RIMZ_REMOTE_MIN_DISPLAY_MS` | Panel grace and minimum display. |
| `RIMZ_REMOTE_PROBE_MS`, `RIMZ_REMOTE_PROBE_TIMEOUT_MS`, `RIMZ_REMOTE_BLACKOUT_MS` | Probe cadence, timeout, and blackout threshold. `RIMZ_REMOTE_PROBE_MS=0` disables probing. |
| `RIMZ_REMOTE_DIAL_MS` | Reachability cadence. `0` disables endpoint discovery and all TCP dials. |
| `RIMZ_REMOTE_TUN` | Forces a TUN interface name for the route classification. |
| `RIMZ_REMOTE_INTERNET_PROBE` | Replaces the public checkpoint with an HTTP or HTTPS URL that must return `204`. Empty or `0` disables the row. |
| `RIMZ_PORTS_SWEEP_MS`, `RIMZ_PROC_NET_DIR` | Remote listener sampling cadence and the procfs root. |

## Invariants worth preserving

- Keep `remote/` pure. Parsing, argv, classification, and state machines belong there with unit tests; processes, threads, clocks, and terminal writes belong in `cli/remote/`.
- Prove the transport before painting success. The background master's `-O check` is the proof; reachability dials are presentation and pacing only.
- Keep the child alive if and only if the master is alive. The pinned `ControlPersist=no`, `ConnectionAttempts=1`, and `ClearAllForwardings=yes` options exist so inherited SSH configuration cannot break that.
- Require all three guards before killing a session as a zombie: established, blacked out, and independently reachable.
- Restore the terminal on every exit path. Snapshot termios at connect, restore after each session, and emit the emulator reset after an unclean end.
- Keep new ack and sidecar fields optional. Both skew directions ride the same connection, and a new field must be invisible to an older peer.
- Let the remote host own the room. Put a new remote capability behind a `rimz` subcommand the snippet execs, so the local side keeps reaching the host through one channel it already supervises.

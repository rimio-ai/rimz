# Remote Attach And Link Health

`rimz remote connect` is a local SSH launcher and link supervisor. The remote host's own `rimz` owns workspace resolution, session birth, the sidebar, and the ledger; the local process owns the SSH child, reconnect policy, and terminal-local link alerts.

## Attach Path

Remote targets use `[user@]host:<session-or-path>`. A bare target after the colon is a session name and compiles to remote `rimz attach --attach`; a value containing `/` or starting with `~` is a path and compiles to remote `rimz start --attach`. The snippet repairs non-login-shell PATH before invoking remote `rimz`, and a missing remote binary exits with the install fix.

The remote PTY carries the local `$TERM`. Portable names such as `xterm-256color`, `screen-256color`, and `tmux-256color` ride through unchanged; terminal-specific names use local `infocmp -x $TERM` and remote `tic` to seed `~/.terminfo` before `exec`, with `TERM=xterm-256color` as the fallback when provisioning cannot run.

The print and one-shot paths use a single `ssh -t` invocation. Every attach enables `-o Compression=yes`; sshd negotiates it and an sshd that disallows compression continues uncompressed. The supervised path adds a PID-scoped ControlMaster socket under the local runtime directory so a second SSH channel can measure the same TCP connection without opening a new transport. The socket directory must be owned by the current user and private (`0700`); when Rimz cannot guarantee that, it skips ControlMaster/probes and attaches with plain SSH.

## Web Access

`rimz remote connect <target> --web` opens a remote Zellij room in the local browser and stays in the foreground supervising the browser tunnel. The local process runs remote `rimz web open --print --json` as a non-PTY prep command, parses the `rimz.web.v1` payload, relays a freshly minted Zellij web token, chooses a stable local port from the session name, starts an SSH `-L 127.0.0.1:<local>:127.0.0.1:<remote>` tunnel, waits for the local port to accept connections, prints `web: http://127.0.0.1:<local>/<session>`, opens the browser best-effort, and then waits until Ctrl-C or tunnel exit.

The prep command is the fail-fast boundary. Remote Rimz without `rimz web`, Zellij without the `web` subcommand, a project whose room is already live under tmux, or a remote room error aborts before browser access opens and surfaces the remote diagnostic. Path targets birth the room; path and exact-session targets both load and grant the presence plugin before asking it to enable sharing at runtime, and the local tunnel command relays the prep command's stderr notes.

The tunnel is a separate SSH child with its own reconnect loop using the same gatetime and backoff as the attach supervisor. `--no-reconnect` applies to that tunnel and exits on a lost established link instead of retrying. `--web-port <port>` pins the local browser origin; without it Rimz hashes the session name into `8300..8399` and scans to the next free port on collision. Ctrl-C stops the foreground Rimz process and the tunnel child.

Remote web uses three SSH connections: prep, token creation, and tunnel. Key or agent authentication gives the intended no-prompt flow; password authentication prompts per connection.

## Reconnect Policy

OpenSSH keepalives stay at `ServerAliveInterval=5` and `ServerAliveCountMax=3`, with `Compression=yes` on the same attach transport, so a hard transport loss reaches exit `255` in about fifteen seconds. A session must live past the gatetime (`30s` by default, `RIMZ_REMOTE_GATETIME_MS` in tests) before exit `255` counts as a dropped established link; an initial auth, host-key, or connect failure is fatal and does not loop a password prompt.

Established transport drops reconnect with capped exponential backoff (`1s` to `30s`, with `RIMZ_REMOTE_BACKOFF_MS` as the test seam). Clean exit `0` returns to the caller. Missing remote `rimz`, remote room failures, and signal death are fatal.

## Link Probe

When reconnect is enabled, the local supervisor starts a long-lived probe stream over the ControlMaster connection:

```text
ssh -S <control-sock> -o BatchMode=yes -- <host> '<PATH repair>; exec rimz remote link-stats ingest --session <name>|--dir <path>'
```

The local probe writes one JSON line every two seconds and the remote ingest command replies with one JSON ack. RTT is an EWMA of probe send-to-ack time, seeded from the second acknowledged probe so the cold remote ingest spawn does not become the first displayed number. Loss is the probe miss percentage over the latest 30 settled probes; this measures the SSH session path rather than ICMP.

The schema is versioned as `rimz.link.v1`. The remote ingest writes `<runtime>/<workspace>/link-stats.json` with temp-file-plus-rename cache semantics, including the remote `received_at_ms`, the SSH client identity, and the latest stats. The sidebar reads that file on every enrichment fold. Stats are fresh for 10 seconds, stale until 120 seconds, and ignored after that. Local rooms never have the file, so their footer is unchanged.

`RIMZ_REMOTE_PROBE_MS=0` disables probing. Probe spawn failures are best-effort; a missing or schema-skewed remote subcommand stops probing without changing the room. The main SSH session is never killed by the probe.

## Bandwidth Attribution

`rimz remote bandwidth --secs 5` samples the room it runs in and reports each pane's process write-rate plus SSH wire-rate when the room has an attached SSH client. Run it on the host serving the room as the room's user: locally for a local room, or inside a remote shell/pane after attaching over SSH. The command resolves the current workspace session, lists panes through the selected backend, pins each pane's root process tree at the start of the window, reads `/proc/<pid>/io` `wchar` before and after the sleep, and prints per-pane bytes/s plus a total. For remote rooms, it also matches the attached mux client's `SSH_CONNECTION` tuple to the room's SSH socket and reads `ss` TCP_INFO counters (`bytes_acked` and `bytes_received`) before and after the same window; those rows are `WIRE(ssh↑)` for egress to the client and `WIRE(ssh↓)` for ingress from the client. Run it without `sudo`, because privilege escalation resets backend and socket resolution away from the room.

The report is best-effort Linux host observability. Hosts without `/proc` return the Linux-host notice. A room whose panes cannot be resolved to root processes returns the no-pane-pids notice; on Zellij the resolver uses the same `/proc` matcher as the sidebar, so active uniquely named foreground commands bind and idle look-alike shells abstain. A room whose root processes resolve but whose `/proc/<pid>/io` entries cannot be read returns the io-unreadable notice; otherwise unreadable child entries are omitted from that pane's sum. The sampler pins the process tree at the first snapshot, so short-lived children born during the window can escape the sample; a longer window catches persistent high-churn TUIs.

The per-pane figures are producer write-rate, including terminal output and non-pty writes such as transcript files. The render path narrows that stream in stages: producers write bytes to ptys, the multiplexer diffs and throttles the focused tab, and SSH compresses the encrypted transport payload. `WIRE(ssh)` is the actual TCP payload on the room's SSH socket after those reductions, so it is normally far below the per-pane sum. `WIRE(ssh)` is omitted for local rooms, rooms without an attached SSH mux client visible in `/proc`, and hosts where `ss` is unavailable.

Tmux fills pane root pids natively. Zellij pane pids are resolved from `/proc` by matching each pane's foreground command inside the session server's process forest, shared with the sidebar metrics path. Tmux can provide a pty-exact future path with `pipe-pane`, but that precision is tmux-only; the current command keeps the backend-parity path by sampling per-pane process trees.

## User Signals

The footer shows a link badge when fresh or stale stats exist: `⇄ remote 210ms` for fresh stats, `⇄ remote …` while the RTT warms, and `⇄ remote ?` for stale stats. Clean links omit loss; the badge appends `{n}%` only when loss is above `10%`.

The badge display is separate from alerting. It renders the worse of latency and loss along the full health ramp: green at RTT `<=100ms` and loss `0%`, through yellow (RTT `200ms` / loss `10%`) and amber (RTT `300ms` / loss `20%`) to bold red (RTT `>=400ms` / loss `>=30%`); a warming badge with no RTT sample stays neutral. Alerting keeps the stepped `LinkTier` thresholds, so low nonzero loss can notify while the continuous badge reads only a touch off green.

The local supervisor emits terminal-local OSC/BEL and matching notification handlers for confirmed link lost and restored edges. Probe blackout emits terminal-local OSC/BEL only, because it is a local stall signal and handlers are reserved for confirmed link drops and recoveries. These alerts are local because a dead link cannot rely on the remote-rendered sidebar to reach the user.

Degraded-but-alive edges, while bytes still flow, surface through the footer badge alone (see [notifications](../sidebar/notifications.md)); they raise no tab bell or notify handler. The remote sidebar still bounds a health episode — ten seconds of fresh degraded or bad stats opens it, thirty seconds of fresh good stats closes it, and stale stats pause both clocks — and writes each open and close as a `link_alert` diagnostic record with the tier, RTT, miss percentage, episode start, and recovery duration when present.

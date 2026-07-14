# Remote Attach And Link Health

`rimz remote connect` is a local SSH launcher and link supervisor. The remote host's own `rimz` owns workspace resolution, session birth, the sidebar, and the store; the local process owns the SSH child, reconnect policy, and terminal-local link alerts.

## Attach Path

Remote targets use `[user@]host:<session-or-path>`. A bare target after the colon is a session name and compiles to remote `rimz attach --attach`; a value containing `/` or starting with `~` is a path and compiles to remote `rimz start --attach`. The snippet repairs non-login-shell PATH before invoking remote `rimz`, and a missing remote binary exits `127` with a setup hint from the remote snippet; the local reconnect supervisor special-cases that sentinel and prints `rimz remote setup <original-input>` instead of the reconnect-policy tail.

`rimz remote setup <alias-or-host>` is a foreground one-shot SSH command that accepts a saved alias, a raw target, or a bare `[user@]host`. The remote snippet does not depend on an existing `rimz`; it runs `uname` on the host, selects the matching prebuilt release archive, downloads it with `SHA256SUMS`, verifies with the platform checksum tool, installs `rimz` to `~/.local/bin/rimz`, and verifies with `~/.local/bin/rimz --version`.

The remote PTY carries the local `$TERM`. Portable names such as `xterm-256color`, `screen-256color`, and `tmux-256color` ride through unchanged; terminal-specific names use local `infocmp -x $TERM` and remote `tic` to seed `~/.terminfo` before `exec`, with `TERM=xterm-256color` as the fallback when provisioning cannot run. When the local terminal advertises 24-bit color, the snippet also exports `COLORTERM=truecolor` because SSH does not forward `COLORTERM`.

The print and one-shot paths use a single `ssh -t` invocation. Every attach enables `-o Compression=yes`; sshd negotiates it and an sshd that disallows compression continues uncompressed. The supervised path adds a PID-scoped ControlMaster socket under the local runtime directory so a second SSH channel can measure the same TCP connection without opening a new transport. The socket directory must be owned by the current user and private (`0700`); when RimZ cannot guarantee that, it skips ControlMaster/probes and attaches with plain SSH.

## Web Access

`rimz remote connect <target> --web` opens a remote Zellij room in the local browser and stays in the foreground supervising the browser tunnel. The local process runs remote `rimz web open --print --json` as a non-PTY prep command with stdin/stderr inherited for recovery prompts, parses the `rimz.web.v1` payload from stdout, relays the cached Zellij web token from the serving machine, chooses a stable local port from the session name, starts an SSH `-L 127.0.0.1:<local>:127.0.0.1:<remote>` tunnel, waits for the local port to accept connections, prints the bare `http://127.0.0.1:<local>/<session>` URL, opens the browser best-effort, and then waits until Ctrl-C or tunnel exit.

The prep command is the fail-fast boundary. Remote RimZ without `rimz web`, Zellij without the `web` subcommand, a project whose room is already live under tmux, or a remote room error aborts before browser access opens and surfaces the remote diagnostic. Path targets birth the room; path and exact-session targets both load and grant the presence plugin before asking it to enable sharing at runtime, and the prep command writes its stderr notes directly to the local terminal.

A prep exit of `127` uses the same missing-binary sentinel as terminal attach and points at `rimz remote setup <original-input>`.

The tunnel is a separate SSH child with its own reconnect loop using the same gatetime, backoff, and network-return accelerator as the attach supervisor. `--no-reconnect` applies to that tunnel and exits on a lost established link instead of retrying. `--web-port <port>` pins the local browser origin; without it RimZ hashes the session name into `8300..8399` and scans to the next free port on collision. Ctrl-C stops the foreground RimZ process and the tunnel child.

Remote web uses three SSH connections: prep, token provisioning, and tunnel. The prep connection carries recovery stdin/stderr; token provisioning prints the cached plaintext token from the serving machine or mints and caches one there. Key or agent authentication gives the intended no-prompt flow; password authentication prompts per connection.

## Reconnect Policy

OpenSSH keepalives stay at `ServerAliveInterval=5` and `ServerAliveCountMax=3`, with `Compression=yes` on the same attach transport, so a hard transport loss reaches exit `255` in about fifteen seconds. A session counts as established once the link probe receives its first ack over the same ControlMaster transport, or once it lives past the gatetime fallback (`30s` by default, `RIMZ_REMOTE_GATETIME_MS` in tests); an early exit `255` after that ack reconnects, while an initial auth, host-key, or connect failure stays fatal and does not loop a password prompt.

Established transport drops reconnect with capped exponential backoff (`1s` to `30s`, with `RIMZ_REMOTE_BACKOFF_MS` as the test seam). Clean exit `0` returns to the caller. Missing remote `rimz`, remote room failures, and signal death are fatal.

At supervisor startup, RimZ runs `ssh -G -- <destination>` once and reads the effective `hostname` and `port`. A configured `ProxyJump` or `ProxyCommand` opts out because a direct dial would not test the path SSH uses; a failed query or unparseable output also keeps the timed reconnect policy unchanged. DNS resolution stays per-dial so a network change can supply a fresh address.

During each retry wait, RimZ quietly dials the effective SSH endpoint every second with a two-second TCP timeout. A wait accelerates only when its first dial was unreachable and a later dial succeeds; an endpoint reachable from the first dial honors the full backoff, and failed dials never lengthen it. A confirmed unreachable-to-reachable transition resets the consecutive-failure counter before the next attach.

An established terminal attach also watches a latched probe blackout. The supervisor kills the SSH child and reconnects immediately only when the session established, its probe has stayed silent past the blackout threshold, and a fresh endpoint dial succeeds; these three guards distinguish a suspend/resume or NAT-rebind zombie from an ordinary slow link. Plain-SSH fallback, disabled probes, probe version skew, and proxied SSH configurations retain OpenSSH keepalive death detection. The web tunnel has no probe stream, so it uses accelerated retry waits without zombie detection.

Hidden seams keep this behavior deterministic in tests: `RIMZ_REMOTE_DIAL_MS` changes the dial cadence and `0` disables both endpoint discovery and dialing; `RIMZ_REMOTE_BLACKOUT_MS` changes the probe blackout threshold. `RIMZ_REMOTE_GATETIME_MS`, `RIMZ_REMOTE_BACKOFF_MS`, `RIMZ_REMOTE_PROBE_MS`, and `RIMZ_REMOTE_PROBE_TIMEOUT_MS` tune the existing establishment, retry, and probe clocks.

## Link Probe

When reconnect is enabled, the local supervisor starts a long-lived probe stream over the ControlMaster connection:

```text
ssh -S <control-sock> -o BatchMode=yes -- <host> '<PATH repair>; exec rimz remote link-stats ingest --session <name>|--dir <path>'
```

The local probe writes one JSON line every two seconds and the remote ingest command replies with one JSON ack. RTT is an EWMA of probe send-to-ack time, seeded from the second acknowledged probe so the cold remote ingest spawn does not become the first displayed number. Loss is the probe miss percentage over the latest 30 settled probes; this measures the SSH session path rather than ICMP.

The schema is versioned as `rimz.link.v1`. The remote ingest writes `<runtime>/<workspace>/link-stats.json` with temp-file-plus-rename cache semantics, including the remote `received_at_ms`, the SSH client identity, and the latest stats. The sidebar reads that file on every enrichment fold. When the probe stream ends, its ingest removes the sidecar if the client identity still names it as the last writer; the 120-second expiry covers hard drops. Stats are fresh for 10 seconds, stale until 120 seconds, and ignored after that. Local rooms never have the file, so their footer is unchanged.

`RIMZ_REMOTE_PROBE_MS=0` disables probing. Probe spawn failures are best-effort; a missing or schema-skewed remote subcommand stops probing without changing the room. A blackout is only one guard in the zombie decision; the supervisor requires independent endpoint reachability before replacing the main SSH session.

## Bandwidth Attribution

`rimz remote bandwidth --secs 5` samples the room it runs in and reports each pane's Linux process write-rate plus SSH wire-rate when the room has an attached SSH client. Run it on the Linux host serving the room as the room's user: locally for a local room, or inside a remote shell/pane after attaching over SSH. The command resolves the current workspace session, lists panes through the selected backend, pins each pane's root process tree at the start of the window, reads `/proc/<pid>/io` `wchar` before and after the sleep, and prints per-pane bytes/s plus a total. For remote rooms, it also matches the attached mux client's `SSH_CONNECTION` tuple to the room's SSH socket and reads `ss` TCP_INFO counters (`bytes_acked` and `bytes_received`) before and after the same window; those rows are `WIRE(ssh↑)` for egress to the client and `WIRE(ssh↓)` for ingress from the client. Run it without `sudo`, because privilege escalation resets backend and socket resolution away from the room.

The report is best-effort Linux host observability because write-rate needs Linux VFS accounting; macOS process disk counters intentionally stay out because they would misrepresent terminal output. Hosts without Linux write-rate counters return the unavailable notice. A room whose panes cannot be resolved to root processes returns the no-pane-pids notice; on Zellij the resolver uses the same process matcher as the sidebar, so active uniquely named foreground commands bind and idle look-alike shells abstain. A room whose root processes resolve but whose `/proc/<pid>/io` entries cannot be read returns the io-unreadable notice; otherwise unreadable child entries are omitted from that pane's sum. The sampler pins the process tree at the first snapshot, so short-lived children born during the window can escape the sample; a longer window catches persistent high-churn TUIs.

The per-pane figures are producer write-rate, including terminal output and non-pty writes such as transcript files. The render path narrows that stream in stages: producers write bytes to ptys, the multiplexer diffs and throttles the focused tab, and SSH compresses the encrypted transport payload. `WIRE(ssh)` is the actual TCP payload on the room's SSH socket after those reductions, so it is normally far below the per-pane sum. `WIRE(ssh)` is omitted for local rooms, rooms without an attached SSH mux client visible in the process table, and hosts where `ss` is unavailable.

Tmux fills pane root pids natively. Zellij pane pids are resolved from the process table by matching each pane's foreground command inside the session server's process forest, shared with the sidebar metrics path. Tmux can provide a pty-exact future path with `pipe-pane`, but that precision is tmux-only; the current command keeps the backend-parity path by sampling per-pane process trees.

## User Signals

The footer shows a link badge when fresh or stale stats exist: `⇄ remote 210ms` for fresh stats, `⇄ remote …` while the RTT warms, and `⇄ remote ?` for stale stats. Clean links omit loss; the badge appends `{n}%` only when loss is above `10%`.

The badge display is separate from alerting. It renders the worse of latency and loss along the full health ramp: green at RTT `<=100ms` and loss `0%`, through yellow (RTT `200ms` / loss `10%`) and amber (RTT `300ms` / loss `20%`) to bold red (RTT `>=400ms` / loss `>=30%`); a warming badge with no RTT sample stays neutral. Alerting keeps the stepped `LinkTier` thresholds, so low nonzero loss can notify while the continuous badge reads only a touch off green.

The local supervisor emits terminal-local OSC/BEL and matching notification handlers for confirmed link lost and restored edges. Probe blackout emits terminal-local OSC/BEL only, because it is a local stall signal and handlers are reserved for confirmed link drops and recoveries. These alerts are local because a dead link cannot rely on the remote-rendered sidebar to reach the user.

Degraded-but-alive edges, while bytes still flow, surface through the footer badge alone (see [notifications](./sidebar/notifications.md)); they raise no tab bell or notify handler. The remote sidebar still bounds a health episode — ten seconds of fresh degraded or bad stats opens it, thirty seconds of fresh good stats closes it, and stale stats pause both clocks — and writes each open and close as a `link_alert` diagnostic record with the tier, RTT, miss percentage, episode start, and recovery duration when present.

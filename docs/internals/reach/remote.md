# Remote Attach And Link Health

`rimz remote connect` is a local SSH launcher and link supervisor. The remote host's own `rimz` owns workspace resolution, session birth, the sidebar, and the ledger; the local process owns the SSH child, reconnect policy, and terminal-local link alerts.

## Attach Path

Remote targets use `[user@]host:<session-or-path>`. A bare target after the colon is a session name and compiles to remote `rimz attach --attach`; a value containing `/` or starting with `~` is a path and compiles to remote `rimz start --attach`. The snippet repairs non-login-shell PATH before invoking remote `rimz`, and a missing remote binary exits with the install fix.

The print and one-shot paths use a single `ssh -t` invocation. The supervised path adds a PID-scoped ControlMaster socket under the local runtime directory so a second SSH channel can measure the same TCP connection without opening a new transport. The socket directory must be owned by the current user and private (`0700`); when Rimz cannot guarantee that, it skips ControlMaster/probes and attaches with plain SSH.

## Reconnect Policy

OpenSSH keepalives stay at `ServerAliveInterval=5` and `ServerAliveCountMax=3`, so a hard transport loss reaches exit `255` in about fifteen seconds. A session must live past the gatetime (`30s` by default, `RIMZ_REMOTE_GATETIME_MS` in tests) before exit `255` counts as a dropped established link; an initial auth, host-key, or connect failure is fatal and does not loop a password prompt.

Established transport drops reconnect with capped exponential backoff (`1s` to `30s`, with `RIMZ_REMOTE_BACKOFF_MS` as the test seam). Clean exit `0` returns to the caller. Missing remote `rimz`, remote room failures, and signal death are fatal.

## Link Probe

When reconnect is enabled, the local supervisor starts a long-lived probe stream over the ControlMaster connection:

```text
ssh -S <control-sock> -o BatchMode=yes -- <host> '<PATH repair>; exec rimz remote link-stats ingest --session <name>|--dir <path>'
```

The local probe writes one JSON line every two seconds and the remote ingest command replies with one JSON ack. RTT is an EWMA of probe send-to-ack time, seeded from the second acknowledged probe so the cold remote ingest spawn does not become the first displayed number. Loss is the probe miss percentage over the latest 30 settled probes; this measures the SSH session path rather than ICMP.

The schema is versioned as `rimz.link.v1`. The remote ingest writes `<runtime>/<workspace>/link-stats.json` with temp-file-plus-rename cache semantics, including the remote `received_at_ms`, the SSH client identity, and the latest stats. The sidebar reads that file on every enrichment fold. Stats are fresh for 10 seconds, stale until 120 seconds, and ignored after that. Local rooms never have the file, so their footer is unchanged.

`RIMZ_REMOTE_PROBE_MS=0` disables probing. Probe spawn failures are best-effort; a missing or schema-skewed remote subcommand stops probing without changing the room. The main SSH session is never killed by the probe.

## User Signals

The footer shows a link badge when fresh or stale stats exist: `⇄ remote 210ms` for fresh stats, `⇄ remote …` while the RTT warms, and `⇄ remote ?` for stale stats. Clean links omit loss; the badge appends `{n}%` only when loss is above `10%`.

The badge display is separate from alerting. It renders the worse of latency and loss along the health ramp's warm tail: calm soft gray at RTT `<=150ms` and loss `<=10%`, then a continuous slide from gold (just past those thresholds) through amber (RTT `300ms` / loss `20%`) to bold red (RTT `>=500ms` / loss `>=30%`). Alerting keeps the stricter `LinkTier` thresholds, so low nonzero loss can notify while the badge stays visually calm.

The local supervisor emits terminal-local OSC/BEL and `[notifications].command` alerts for confirmed link lost and restored edges. Probe blackout emits terminal-local OSC/BEL only, because it is a local stall signal and the configured command is reserved for confirmed link drops and recoveries. These alerts are local because a dead link cannot rely on the remote-rendered sidebar to reach the user.

The remote sidebar handles degraded-but-alive edges while bytes still flow: ten seconds of fresh degraded or bad stats raises `link_degraded`, thirty seconds of fresh good stats raises `link_recovered`, and stale stats pause both clocks. Each active/recovered episode also writes a `link_alert` diagnostic record with the tier, RTT, miss percentage, episode start, and recovery duration when present.

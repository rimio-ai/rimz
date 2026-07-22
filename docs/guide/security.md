# Security and Trust

RimZ runs as you: no daemon, no elevated privileges, no account of its own. It reports on the agents you already run, inside the terminal you already use. Its security model follows from that. RimZ asks before it changes anything on your machine, every change has a matching undo, and the only places a config file can make RimZ run a command are decisions you make by hand.

This page covers three questions: what RimZ touches, what can execute, and what leaves your machine. The design commitments behind them are in [DESIGN.md](../../DESIGN.md).

## What RimZ changes on your machine

Three things, each previewable before it happens and reversible after.

**Reporting hooks in your agent configs.** To show an agent live in the sidebar, RimZ adds hook lines to that agent's own config (`~/.claude/settings.json`, `~/.codex/config.toml`, and the like). Run `rimz hooks install --dry-run` for the exact per-agent diff before anything is written. The install is additive, so your existing hooks stay, and each hook is one `rimz hooks feed` line that reports events. Answering a prompt always stays with you, in the agent's own UI. `rimz hooks uninstall` removes exactly what was added and restores any statusline RimZ wrapped. The whole flow is walked in [set up your machine](./setup.md#install-agent-hooks).

**Files under your home directory.** Configuration lives under `~/.config/rimz/`, durable room state under `~/.local/state/rimz/`, both plain files you can read and edit. `rimz uninstall` removes hooks, rooms, runtime state, and the binary, with flags to also drop the stores and config ([maintenance reference](../reference/cli/maintenance.md#update-reload-reset-gc-and-uninstall)).

**Disposable Copilot telemetry.** A newly-born room can hold `agent-telemetry/copilot-otel.jsonl` under its mode-`0700` runtime root. RimZ forces message-content capture off for this managed exporter and reads only allow-listed model/token metadata for the exact Copilot conversation. Reset and runtime GC remove the cache; RimZ neither rotates a live exporter file nor derives account history from it. An explicit user exporter remains user-owned and takes precedence.

**A permission grant for its Zellij plugin.** On Zellij, RimZ seeds a permission grant for the presence plugin it ships, so the first attach runs without an interrupting prompt. Your `config.kdl` stays untouched, and the grant is yours to revoke. Details in [The Zellij presence plugin](#the-zellij-presence-plugin).

## What can run commands

Inside a workspace, plenty already runs as you: hooks, postinstall scripts, generated binaries, test runners, and the agents themselves. Same-user isolation is no real boundary there, so RimZ does not lean on it. Instead it makes command execution an explicit choice in exactly two places.

### Project trust

A cloned repository can ship a `.rimz/config.toml` that names agents, profiles, teams, loop tasks, hooks, and environment variables, and any of those can run a command. So RimZ keeps the whole file inert until you trust the workspace: on an untrusted clone it reads only structural metadata, and nothing the file declares can launch.

A first interactive `rimz start` in an untrusted workspace offers the grant, and the interactive `rimz loop fire` and project-task edit surfaces offer it inline with the surface diff when their trust gate is closed. Declining leaves the workspace inert; only the first-start prompt stays suppressed until `.rimz/config.toml` changes.

`rimz trust grant` pins a single hash over every command-running field in that config. Each later read re-hashes the live file. Edits outside RimZ's project-task commands, including direct file edits by an agent in the room, make the hash drift: the workspace flips to `stale`, command execution turns back off, and `rimz trust status` prints a field-level diff of what changed before you re-grant. When `rimz loop add`, `remove`, or `rename` performs the edit from a trusted workspace or creates the first config, it re-pins the grant whether invoked interactively, by a script, or by an agent; an untrusted or stale pre-state keeps the review flow. There is no background sweep. `rimz trust status` and `rimz doctor` both re-hash on the spot.

The hashed surface is every field that can cause a process to run:

- `[[agents]]`: `name`, `launch_command`, `env`
- `[profiles.<name>]`: `agent`, `mode`, `model`, `effort`, `system-prompt-file`, `append-system-prompt-file`, `args`
- `[agents.teams.<name>]`: `layout`, and each role's profile and launch fields
- `[tasks.<name>]`: the loop `agent`, `prompt`, `check` and `verify` commands, and the run and schedule options
- `[[hooks]]`: `event`, `command`
- `[env]`: every key and value

Room layout stays out of a repo's reach: a project config carrying a `[layout]` table (including tmux status `#(...)` and popup commands) is refused, with the fix to move it to your per-machine config. Any field RimZ can execute must enter this hash, and a unit test fails if one slips out. The mechanics, the grant record, and the diff are in [the trust internals](../internals/harness/trust.md); the command is in [the trust reference](../reference/cli/hooks-trust.md#project-trust).

Per-machine loop schedules are separate: a `check = "<shell>"` line in your own `~/.config/rimz/loop.toml` is your command, not a repo's. A clone can supply only project `[tasks]`, and those stay inert until you grant, unless a same-named machine task is already the one that runs.

### Notification handlers

Notification handlers run a command of your choosing when a row needs attention (`[[notifications.handler]]`, or the legacy `[notifications].command`). They live only in your per-machine `~/.config/rimz/config.toml`, so a clone can never supply one. They run under your user id, spawned by the sidebar process, and often carry local push credentials. A handler that acts back on the room should treat pane text and transcripts as untrusted data: match a bounded prompt shape, and stay silent on anything else. Wiring is in [the notifications internals](../internals/sidebar/notifications.md).

### The Zellij presence plugin

On Zellij, RimZ loads a small presence plugin into each session so the sidebar learns pane layout by push, and a tab switch lands back on your work instead of the sidebar ([internals](../internals/multiplexers.md#the-zellij-presence-plugin)). Zellij normally prompts before a plugin gets permissions; RimZ seeds that grant ahead of load, keyed to the exact plugin path it materializes, so the first attach is not interrupted. The grant covers:

- **Access Zellij state**: the plugin watches pane and tab shape.
- **Run commands**: it runs the RimZ-owned `rimz sidebar wake` and `rimz sidebar focus` calls, and nothing else.
- **Reconfigure**: it applies the room's mouse options and, if you set one, binds the [focus key](./configuration.md#sidebar-rendering), both at runtime without writing your `config.kdl`.
- **Start web server**: added only when `[web] enabled`, so `rimz web open` can share a running session.

The plugin's code, argv, and configuration are all RimZ-owned, never your `config.kdl`, and it ships no pane content anywhere. The grant lives in Zellij's own permission store, where its plugin manager can revoke it; revoking stops pane discovery until you restore it, and `rimz doctor` names the fix.

### Browser listener

Browser access defaults to a loopback ttyd listener with one machine-wide Basic-Auth credential. That credential reaches every live RimZ room on the machine and grants a shell as the serving user, so handle it like a machine-wide SSH key.

`[web] auth_header` delegates the public decision to an authenticating reverse proxy. RimZ's gate validates the proxy source, requires exactly one non-empty header on every request, optionally matches its trimmed value exactly and case-sensitively against `[web] auth_users`, strips client-supplied `Authorization`, and injects the machine's Basic credential before forwarding to ttyd. The identity provider's application policy remains the primary access control; `auth_users` provides defense in depth using canonical username spellings. The proxy strips client-supplied copies of the identity header, injects its own value after authentication, and remains the only non-loopback source that can reach the gate.

`[web] trusted_proxies` admits matching IPv4 or IPv6 CIDRs to this gate; an empty list leaves only loopback sources admitted. Pair the CIDRs with the host firewall, and account for the source address produced by container or host networking. Loopback bypasses only the source-address check: trusted-header requests still need the configured header, and the private ttyd listener still needs Basic Auth. On a multi-user host, use host-level user isolation when another local user can read the serving user's credential or execute as that user.

The [web guide](./web.md#behind-a-reverse-proxy) configures Traefik and Authentik for this path.

## What leaves your machine

RimZ keeps your work local. Your prompts, transcripts, pane text, file paths, and credentials stay on the box. The network calls RimZ makes reuse logins you already hold and reach only services you already use:

- **Provider usage.** To fill the cost and budget meters, RimZ reads each provider's usage endpoint with the OAuth login the agent already holds, contacting only the provider you are signed in to.
- **Pull-request status.** The sidebar's PR marker runs your own `gh` (GitHub) or `tea` (Gitea, Forgejo, Codeberg) against your forge, on your existing login. It reads no RimZ secrets and adds no config field, so it stays off the trust hash.
- **Pets.** Enabling `[theme.pets]` fetches a sprite sheet over HTTPS from the host you name (a built-in pet reaches the public Codex pets CDN; a local-path pet fetches nothing). `RIMZ_PETS_OFFLINE=1` makes the process tree cache-only. The request carries the asset URL and nothing else.
- **Off-box error reporting.** Off by default and opt-in. See [Off-box error reporting](#off-box-error-reporting).

### Hook payload privacy

Hook payloads can carry prompts, tool inputs, file paths, command arguments, and errors. They stay in RimZ's local state, and nothing forwards them off the box. Payload-fidelity and retention controls (a `[privacy]` block with `payload_mode` and `retention_days`) are a planned project surface, tracked in [configuration.md](./configuration.md) against the [hook adapter boundary](../internals/agents/adapter.md#the-hook-path).

### Off-box error reporting

Release binaries ship without the reporting code, so a stock install makes no Sentry calls whatever the config. Reporting compiles in only under `--features sentry`, and even then it is off until you set `[sentry] dsn` (or `RIMZ_SENTRY_DSN`). The DSN is per-machine and never the committed project config, so a clone or pull cannot redirect a contributor's telemetry.

When on, it sends RimZ's own `warn!` and `error!` events and the provider conditions RimZ observes (rate limits, overload), tagged with low-cardinality facts: the release and build id, the running command, a fault class, and the agent or session id when known. The hostname is stripped, Sentry's default personal-data collection is off, and hook payloads, prompts, and transcripts never ride along. A failed account-usage probe reports the request's host, never its path or query. The full payload and the config knobs are in [the diagnostics internals](../internals/diagnostics.md#off-box-error-reporting) and [configuration.md](./configuration.md#off-box-error-reporting).

## Pane text is data, not instructions

`rimz pane capture` returns raw terminal text. RimZ core treats it as data: it never parses it for correctness and never types on your behalf. If you script an answer through the pane primitives, match a bounded prompt shape and abstain when unsure. Feeding captured text into an LLM prompt as if it were a user message is the classic prompt-injection footgun, so keep it as data.

## Unattended runs

An unattended run still decides permissions the agent's own way: either you keep every native prompt (each becomes a waiting row that routes to you) or you pass the agent's own bypass flag. The tradeoffs, and what an out-of-range agent version does to the prompt path, are in [the loops permission posture](./loops.md#the-permission-posture-for-unattended-runs).

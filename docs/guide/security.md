# Security and trust

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

## Threat model

A project workspace runs untrusted code. Hooks, postinstall scripts, generated binaries, test runners, and the agents themselves all execute as you. Same-UID isolation is therefore not a meaningful trust boundary inside a workspace. Trust must be explicit at two layers:

1. **Project trust** — what Rimz reads from `.rimz/config.toml` and what it is allowed to execute on the project's behalf.
2. **Resolver allowlist** — what is allowed to answer feed items on your behalf.

These are the only two trust decisions Rimz asks you to make. Everything else flows from them.

## Project trust

Project config is read inertly until trusted.

**Untrusted.**
- Structural metadata only.
- No project-declared commands run.
- Project-declared profiles stay inert.
- No project-declared hook installs proceed.
- No project-launched resolver binaries start.

**Trusted.**
- Full project config applies.
- The executable-surface hash matches the trusted hash.

**Trust stale.**
- Executable-surface hash changed since the last grant.
- Command-running fields are disabled until trust is granted again.
- Auto-revoke is implicit: every `rimz trust status` and `rimz doctor` re-hashes the live `.rimz/config.toml` and reports `stale` without a separate sweep.

The **executable surface** is every project field that can cause a process to run: agent launch commands, project profile definitions, hook commands, PATH-affecting env overrides, layout-launched commands, tmux status `#(...)`, tmux popup `display-popup -E`, and any future project command string. A single hash over all of these is what `rimz trust grant` pins. Adding a new project command-running field that isn't in the hash is a CI invariant violation. Implementation detail in [`docs/internals/sidebar/trust.md`](../internals/sidebar/trust.md).

Per-machine notification handlers (`[[notifications.handler]]` and legacy `[notifications].command`) live in `~/.config/rimz/config.toml`, outside project trust. They are personal routing on this host, often with local push credentials, and a cloned repository never supplies them.

The per-machine `[agents.loop.tasks]` schedules also live outside project trust: each entry runs only the rimz-owned `rimz loop run`, never arbitrary shell, so a clone supplies nothing executable. The scheduled-execution surface stays visible — `rimz loop add` runs hook preflight before recording a task, `rimz loop list` shows whether the task's room is open, and `rimz doctor` carries the configured tasks.

## Resolver trust

Resolver trust is a per-machine allowlist. A same-UID process can write a heartbeat file, but only enrolled `resolver_id`s engage the bridge. Heartbeats from unknown resolver IDs are kept for diagnostics; `rimz doctor` reports them as `unauthorized resolver heartbeat seen`.

Optional `--binary <path>` pins a resolver's executable path; Rimz then verifies the heartbeating process's executable matches before engaging the bridge. `rimz doctor` reports when platform support degrades that check.

Project config that *launches* a resolver binary flows through the project trust gate first. The two gates layer: project trust controls whether project config can launch a resolver at all; the resolver allowlist controls whether a heartbeating resolver can answer once launched.

Detail in [resolvers.md](../internals/agents/resolvers.md).

## Hook safety

The mechanics behind these guarantees — the decision channel, the neutral no-op, fresh stdio — are in [agent.md → Hook stdout is the decision channel](../internals/agents/agent.md#hook-stdout-is-the-decision-channel).

- Hook stdout is reserved for the agent's decision channel.
- Logs go to stderr or Rimz runtime state logs such as `binding.log.jsonl`.
- Notification helpers do not run inside the blocking hook process.
- Hook child processes must not inherit stdout. CI grep enforces this.
- Every neutral and decision payload is golden-tested.

## UID boundaries

An agent launched through `sudo`, `su`, or `doas` as another real uid is visible as a foreign process, not as a Rimz agent. The sidebar may label the process row with the agent kind and uid marker, but hooks, hook installation, account probes, and resolver delegation remain scoped to the current uid and the trusted project surface. This keeps another user's `~/.claude` or equivalent config and credentials outside the current room's trust decision.

## Forge status probes

The sidebar's PR marker is best-effort enrichment: the producer runs `gh` for GitHub remotes or `tea` for Gitea/Forgejo/Codeberg remotes, in the worktree, on a long TTL. The CLI uses your existing forge login and contacts the repository's own forge to list PR state for the current branch; unsupported forges and branches without PRs publish an empty cache. The probe reads no Rimz secrets and adds no project config field, so it stays outside the project trust hash.

## Pane safety

`rimz pane capture` returns untrusted terminal text. Rimz core does not parse it for correctness and does not auto-type. Resolvers that use pane primitives must pattern-match bounded prompt shapes and abstain when unsure. Captured text is data, never an instruction stream — feeding it into an LLM prompt as if it were a user message is the standard prompt-injection footgun.

## The Zellij presence plugin

On Zellij, rimz loads a small presence plugin into each session so the sidebar learns of pane changes by push instead of polling and tab switches land back on work instead of the sidebar ([internals](../internals/sidebar/multiplexers.md#zellij-presence-channel)). The first load surfaces Zellij's own permission prompt, once, for **Access Zellij state** (it watches pane/tab shape), **Run commands** (it runs `rimz sidebar wake`, a fixed argv rimz pins at load), and **Reconfigure** (it applies Rimz's room mouse options and, when configured, binds the [focus key](../reference/configuration.md#sidebar-rendering) to a runtime-only plugin pipe without writing your `config.kdl`). Approve with `y` and the prompt pane closes itself; Zellij remembers the grant across sessions and restarts, keyed to the plugin path rimz materializes under the user data directory. The plugin reports a switched-to tab that restored focus to the sidebar; the matching renderer moves focus through the same host command used for an ordinary sidebar jump. Declining **Run commands** costs latency and tab-focus correction — the sidebar falls back to its poll and Zellij keeps its native remembered focus; declining **Reconfigure** leaves birth-time mouse options in place and skips the auto-bound focus key, which a hand-written `config.kdl` bind still covers — and `rimz doctor` shows which mode a workspace is in. The plugin's argv, artifact, and configuration are all rimz-owned (never your `config.kdl`), it ships no pane content anywhere, and the grant stays in Zellij's own permission store where its plugin manager can revoke it.

## State safety

- State directories use `0700` permissions.
- Feed resolution requires workspace ID, request ID, and nonce.
- First valid CAS writer wins. Later writers are rejected, or recorded as `late audit` where the state machine allows.
- PID identity is cleanup metadata only — never the basis for authorization.
- The session identity pin (`RIMZ_WORKSPACE_ID`/`RIMZ_PROJECT_ROOT`) selects which ledger a participant writes to; it executes nothing and enters no trust hash. The pin is hash-verified against its root, and same-UID environment access sits inside the existing trust boundary — a forged pin can redirect a write only to a ledger the same user already owns.

## Privacy

Hook payloads can include prompts, tool inputs, file paths, command arguments, and errors. Project privacy config controls retention and payload fidelity:

```toml
[privacy]
retention_days     = 14
payload_mode       = "redacted"   # metadata | redacted | full
max_payload_bytes  = 8192
```

- `metadata` — strips inputs, prompts, args, errors. Smallest footprint.
- `redacted` — keeps bounded payloads with built-in redaction. Default.
- `full` — keeps hook payloads as delivered. `rimz doctor` warns.

## Optional asset fetches

Enabling `[theme.pets]` lets the sidebar fetch a WebP sprite sheet over HTTPS and cache it under the per-machine cache root. A built-in `pet` reaches the public Codex pets CDN; an `https://` URL reaches the host you name (plaintext `http://` is rejected); a petdex pet (`~/.codex/pets/<name>/`) or a local-path `pet` fetches nothing and reads straight off disk. `RIMZ_PETS_OFFLINE=1` makes the process tree cache-only. The fetch sends the asset URL request; prompts, transcripts, pane text, workspace paths, and provider credentials stay local. Pets execute no commands, so the setting stays outside the project trust hash.

## Off-box error reporting

Setting `[sentry] dsn` in the per-machine config (or `RIMZ_SENTRY_DSN`) routes Rimz diagnostics to a Sentry project in builds compiled with `--features sentry`. Release binaries ship without the reporting code, so a production binary makes no Sentry calls regardless of config. In opted-in builds it is off by default: with no DSN, no client is created and Rimz makes no network calls. The target is a per-machine setting and never the committed project config, so a clone or pull cannot redirect a contributor's telemetry to a foreign endpoint, and the DSN stays off the project trust surface.

When on, Rimz sends its own `warn!`/`error!` events and the agent turn-error warning (the rate-limit, overload, and other provider conditions Rimz observes, reported at warning level). Those payloads carry Rimz error text and a stacktrace, file paths that appear in errors, the `rimz@<build>` release, the running command and build id, the `fault` class (`agent` for an observed provider condition, `rimz` for a Rimz fault), the agent kind, the session id and turn-error class, the operation that failed, and — for a failed account-usage probe — the request's host authority (never its path or query); a `workspace` tag scopes them to a repository, and a curated set of breadcrumb seeds trails an event with the steps before it (only `info!` lines marked for the trail, so a stray field never rides along). The hostname is stripped and Sentry's default PII is off; hook payloads, prompts, and transcripts are never forwarded. Reporting is best-effort enrichment — a malformed DSN logs the fix and stays off, and a network failure never blocks a Rimz path.

## Version drift

When an agent version is outside the tested range:

- observability hooks may remain active,
- the decision bridge disables by default,
- blocking feed hooks pass through with neutral output (agent's native UI takes over),
- `--unsafe-agent-version` can override per workspace; `rimz doctor` keeps warning.

For the two unattended-run patterns (agent-native bypass vs permissive resolver) and their audit tradeoffs, see [product.md](./product.md).

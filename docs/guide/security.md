# Security and trust

Rimz runs beside your agents with the permissions you already have, so its security posture is visibility and consent: every change it makes to your machine is previewable and reversible, and every place configuration can run a command is an explicit decision you make. This page answers the two questions that matter on day one — what Rimz touches, and what can execute — then states the deeper guarantees. The design commitments behind them are in [DESIGN.md](../../DESIGN.md).

## What Rimz changes, and how to undo it

Rimz asks before it writes, and everything it writes has a matching undo.

- **Reporting hooks in your agent configs.** The first run (or `rimz hooks install`) names each agent and the exact config file it will touch, and `rimz hooks install --dry-run` prints the full diff before anything is written. The install is additive — your existing hooks stay — and each hook is one `rimz hooks feed` line that reports events; answering a prompt always stays with you, in the agent's own UI. `rimz hooks uninstall` removes exactly what was added and restores your statusline.
- **Per-machine files it owns.** Config lives under `~/.config/rimz/` and durable room state under `~/.local/state/rimz/`. `rimz uninstall` removes hooks, rooms, runtime state, and binaries, with flags to include stores and config ([maintenance reference](../reference/cli/maintenance.md#reload-reset-gc-and-uninstall)).
- **A permission grant for its own Zellij plugin.** On Zellij, Rimz seeds the permission cache for the presence plugin it ships; the scope and the revocation path are below in [The Zellij presence plugin](#the-zellij-presence-plugin). Your `config.kdl` is never edited.

## What can run commands, and when you approve it

Two configuration surfaces can cause a process to run, and each is an explicit trust decision:

1. **Project trust** — what Rimz reads from a repository's `.rimz/config.toml` and what it is allowed to execute on the project's behalf.
2. **Notification handlers** — the per-machine commands you wire to run on the room's attention cues.

These are the only two trust decisions Rimz asks you to make. Everything else flows from them.

### Project trust

Project config is read inertly until trusted.

**Untrusted.**
- Structural metadata only.
- No project-declared commands run.
- Project-declared profiles stay inert.
- No project-declared hook installs proceed.

**Trusted.**
- Full project config applies.
- The executable-surface hash matches the trusted hash.

**Trust stale.**
- Executable-surface hash changed since the last grant.
- Command-running fields are disabled until trust is granted again.
- Auto-revoke is implicit: every `rimz trust status` and `rimz doctor` re-hashes the live `.rimz/config.toml` and reports `stale` without a separate sweep.

The **executable surface** is every project field that can cause a process to run: agent launch commands, project profile and team definitions, project loop tasks (`spec`, `prompt`, `check`, and launch options), hook commands, PATH-affecting env overrides, and any future project command string. `rimz trust grant` pins a single hash over all of these. The grant also stores the surface itself, so a `stale` report shows a field-level diff of what changed before you re-grant. Room layout (`[layout]`, including tmux status `#(...)` and popup commands) is per-machine config; a project config carrying it is refused with the move-it fix. Adding a new project command-running field that isn't in the hash is a CI invariant violation. Implementation detail in [`docs/internals/harness/trust.md`](../internals/harness/trust.md).

The per-machine `loop.toml` schedules live outside project trust: a `check = "<shell>"` entry there is your own scheduled command, stored under `~/.config/rimz/` or Rimz-owned state. A cloned repository can supply only project `[tasks]` in `.rimz/config.toml`. Those tasks enter the trust hash, stay visible as untrusted or stale, and refuse to run until you grant — unless a same-named machine task is the effective runnable task. The scheduled-execution surface stays visible: `rimz loop add` runs hook preflight before recording an agent action, `rimz loop list` shows source and room state, and `rimz doctor` carries the configured tasks.

### Notification handlers

Notification handlers are per-machine commands (`[[notifications.handler]]` and the legacy `[notifications].command`), entering the room only by your hand in `~/.config/rimz/config.toml`; a cloned repository can never supply them. They are personal routing on this host, often with local push credentials, and they run under your uid, spawned by the sidebar process that holds the room's refresh duty.

A handler that acts on the room treats pane text and transcripts as untrusted data: match bounded prompt shapes, and stay silent on anything unknown. Wiring detail in [notifications.md](../internals/sidebar/notifications.md).

### The Zellij presence plugin

On Zellij, Rimz loads a small presence plugin into each session so the sidebar learns pane topology by push and tab switches land back on work instead of the sidebar ([internals](../internals/multiplexers.md#zellij-presence-channel)). Rimz seeds Zellij's own permission cache for this plugin before load, keyed to the canonical plugin path Rimz materializes under the user data directory, so no prompt interrupts the first attach. The seeded grant covers:

- **Access Zellij state** — the plugin watches pane and tab shape.
- **Run commands** — it runs the Rimz-owned `rimz sidebar wake` and `rimz sidebar focus` argv.
- **Reconfigure** — it applies Rimz's room mouse options and, when configured, binds the [focus key](./configuration.md#sidebar-rendering) to a runtime-only plugin pipe, without writing your `config.kdl`.
- **Start web server / share session** — added only when `[web] enabled`; it lets browser access turn on when you run `rimz web open` against an already-running session.

The plugin's argv, artifact, and configuration are all Rimz-owned — never your `config.kdl` — and it ships no pane content anywhere. The grant stays in Zellij's own permission store, where its plugin manager can revoke it; revoking makes pane discovery unavailable until the grant is restored, and `rimz doctor` names the fix. Setting `[web] enabled = false` stops Rimz from seeding the web grant and makes web commands fail fast before changing room sharing. The plugin also reports a switched-to tab that restored focus to the sidebar, and the renderer moves focus back through the same host command an ordinary sidebar jump uses.

## Threat model

A project workspace runs untrusted code: hooks, postinstall scripts, generated binaries, test runners, and the agents themselves all execute as you. Same-UID isolation is therefore not a meaningful trust boundary inside a workspace. That is why trust is explicit at the two layers above — the project's executable surface and your per-machine handlers — and why everything read back from a pane or transcript is treated as data, never as instructions ([Pane safety](#pane-safety)).

## Hook safety

The mechanics behind these guarantees — the decision channel, the neutral no-op, fresh stdio — are in [agent.md → Hook stdout is the decision channel](../internals/agents/model.md#hook-stdout-is-the-decision-channel).

- Hook stdout is reserved for the agent's decision channel.
- Logs go to stderr or Rimz runtime state logs such as `binding.log.jsonl`.
- Notification helpers do not run inside the blocking hook process.
- Hook child processes must not inherit stdout. CI grep enforces this.
- Every neutral and decision payload is golden-tested.

## Pane safety

`rimz pane capture` returns untrusted terminal text. Rimz core does not parse it for correctness and does not auto-type. A script that answers prompts through pane primitives must pattern-match bounded prompt shapes and abstain when unsure. Captured text is data, never an instruction stream — feeding it into an LLM prompt as if it were a user message is the standard prompt-injection footgun.

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

## UID boundaries

An agent launched through `sudo`, `su`, or `doas` as another real uid is visible as a foreign process, not as a Rimz agent. The sidebar may label the process row with the agent kind and uid marker, but hooks, hook installation, account probes, and notification handlers remain scoped to the current uid and the trusted project surface. This keeps another user's `~/.claude` or equivalent config and credentials outside the current room's trust decision.

## Forge status probes

The sidebar's PR marker is best-effort enrichment: the sidebar runs `gh` for GitHub remotes or `tea` for Gitea/Forgejo/Codeberg remotes, in the worktree, on a repo-tier TTL. The CLI uses your existing forge login and contacts the repository's own forge to list the repo's open PRs, then matches branch names locally; an open PR that disappears from that set gets one targeted branch lookup to resolve closed versus merged. Unsupported forges and branches without PRs publish an empty cache. The probe reads no Rimz secrets and adds no project config field, so it stays outside the project trust hash.

## CI build cache

CI build and test workflows run PR code on the `pull_request` trigger, so fork PRs receive an isolated Actions-cache scope: they can restore base warm entries and write only their PR scope for same-PR re-runs. Release jobs set `RIMZ_SCCACHE=off`, so published archives are built from fresh rustc outputs in the release target directory.

## State safety

- State directories use `0700` permissions.
- A supervised-run wakeup counts only when its workspace ID and run ID match the waiting run's socket.
- PID identity is cleanup metadata only — never the basis for authorization.
- The session identity pin (`RIMZ_WORKSPACE_ID`/`RIMZ_PROJECT_ROOT`) selects which store a participant writes to; it executes nothing and enters no trust hash. The pin is hash-verified against its root, and same-UID environment access sits inside the existing trust boundary — a forged pin can redirect a write only to a store the same user already owns.

## Off-box error reporting

Setting `[sentry] dsn` in the per-machine config (or `RIMZ_SENTRY_DSN`) routes Rimz diagnostics to a Sentry project in builds compiled with `--features sentry`. Release binaries ship without the reporting code, so a production binary makes no Sentry calls regardless of config. In opted-in builds it is off by default: with no DSN, no client is created and Rimz makes no network calls. The DSN is a per-machine setting and never the committed project config, so a clone or pull cannot redirect a contributor's telemetry to a foreign endpoint, and the DSN stays off the project trust surface.

When on, Rimz sends its own `warn!`/`error!` events and the agent turn-error warning (the rate-limit, overload, and other provider conditions Rimz observes, reported at warning level). Sidebar refresh-health warnings use the local-only `rimz::sidebar::health` target and stay in the durable diagnostics log instead of Sentry. An off-box payload carries:

- Rimz error text with a stacktrace, and the file paths that appear in errors.
- The `rimz@<build>` release, the running command, and the build id.
- The `fault` class (`agent` for an observed provider condition, `rimz` for a Rimz fault), the agent kind, the session id, and the turn-error class.
- The operation that failed — for a failed account-usage probe, the request's host authority, never its path or query.
- A `workspace` tag that scopes the event to a repository.
- A curated breadcrumb trail of the steps before the event — only `info!` lines marked for the trail, so a stray field never rides along.

The hostname is stripped and Sentry's default PII is off; hook payloads, prompts, and transcripts are never forwarded. Reporting is best-effort enrichment — a malformed DSN logs the fix and stays off, and a network failure never blocks a Rimz path.

## Version drift

When an agent version is outside the tested range:

- observability hooks may remain active,
- blocking ask hooks keep returning the agent-native neutral output, so the prompt stays in the agent's UI either way,
- drift degrades observability fidelity only; `rimz doctor` warns.

For the two unattended-run patterns (the agent-native bypass flag vs answering in the agent's own UI) and their audit tradeoffs, see [the loops guide → the permission posture for unattended runs](./loops.md#the-permission-posture-for-unattended-runs).

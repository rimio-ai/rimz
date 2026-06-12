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
- No project-declared hook installs proceed.
- No project-launched resolver binaries start.

**Trusted.**
- Full project config applies.
- The executable-surface hash matches the trusted hash.

**Trust stale.**
- Executable-surface hash changed since the last grant.
- Command-running fields are disabled until trust is granted again.
- Auto-revoke is implicit: every `rimz trust status` and `rimz doctor` re-hashes the live `.rimz/config.toml` and reports `stale` without a separate sweep.

The **executable surface** is every project field that can cause a process to run: agent launch commands, hook commands, PATH-affecting env overrides, layout-launched commands, tmux status `#(...)`, tmux popup `display-popup -E`, and any future project command string. A single hash over all of these is what `rimz trust grant` pins. Adding a new project command-running field that isn't in the hash is a CI invariant violation. Implementation detail in [`docs/internals/sidebar/trust.md`](../internals/sidebar/trust.md).

The per-machine `[notifications].command` lives in `~/.config/rimz/config.toml`, outside project trust. It is personal routing on this host, often with local push credentials, and a cloned repository never supplies it.

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

## Pane safety

`rimz pane capture` returns untrusted terminal text. Rimz core does not parse it for correctness and does not auto-type. Resolvers that use pane primitives must pattern-match bounded prompt shapes and abstain when unsure. Captured text is data, never an instruction stream — feeding it into an LLM prompt as if it were a user message is the standard prompt-injection footgun.

## The Zellij presence plugin

On Zellij, rimz loads a small presence plugin into each session so the sidebar learns of pane changes by push instead of polling and tab switches land back on work instead of the sidebar ([internals](../internals/sidebar/multiplexers.md#zellij-presence-channel)). The first load surfaces Zellij's own permission prompt, once: **Access Zellij state** (it watches pane/tab shape) and **Run commands** (it runs `rimz sidebar wake`, a fixed argv rimz pins at load). Approve with `y` and the prompt pane closes itself; Zellij remembers the grant across sessions and restarts, keyed to the plugin path rimz materializes under the user data directory. The plugin reports a switched-to tab that restored focus to the sidebar; the matching renderer moves focus through the same host command used for an ordinary sidebar jump. Declining costs latency and tab-focus correction — the sidebar falls back to its poll and Zellij keeps its native remembered focus — and `rimz doctor` shows which mode a workspace is in. The plugin's argv, artifact, and configuration are all rimz-owned (never your `config.kdl`), it ships no pane content anywhere, and the grant stays in Zellij's own permission store where its plugin manager can revoke it.

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

## Version drift

When an agent version is outside the tested range:

- observability hooks may remain active,
- the decision bridge disables by default,
- blocking feed hooks pass through with neutral output (agent's native UI takes over),
- `--unsafe-agent-version` can override per workspace; `rimz doctor` keeps warning.

For the two unattended-run patterns (agent-native bypass vs permissive resolver) and their audit tradeoffs, see [product.md](./product.md).

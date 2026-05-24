# Agent integrations

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

Agent integrations are adapters that translate a coding agent's native hook protocol onto Rimz events and feed items. The generic event/feed API is the ground truth; agents are sources. Anything an agent integration does, a shell script can do through the same CLI.

## The integration trait

Every adapter implements:

```text
install_hooks()
uninstall_hooks()
classify_hook()
observe_lifecycle()
render_decision(feed_kind, resolution)
render_neutral(event_name)
hook_cap()
```

Decision renderers are agent-specific. Do not reuse one agent's JSON shape for another — Claude expects `hookSpecificOutput`, Codex expects a bare `{"decision": "allow"}`, and conflating them silently breaks one of them.

## Two hook channels

Adapters wire two kinds of hooks. The distinction is whether the hook can hold the agent open while Rimz waits for an answer.

**Lifecycle hooks — fast, non-blocking.** Drive agent status, mode pills, notifications, and **Recent activity**.

```text
SessionStart   UserPromptSubmit   PreToolUse   PostToolUse
Stop           SessionEnd         Notification
```

**Feed hooks — blocking-capable.** The path the bridge engages.

```text
permission request
plan approval
user question
```

Blocking decision hooks must be **sync**. Installing one as async is a hard error — the agent would ignore the decision printed on stdout. The installer rejects async configs explicitly.

## Status and mode

The agent owns the status and mode vocabulary; Rimz observes and renders. The five-value status set and the five-value mode pill are defined in [DESIGN.md → Sidebar shape](../../DESIGN.md#sidebar-shape).

Bypass is observed from the agent's own flag (`claude --dangerously-skip-permissions`, `codex --ask-for-approval never`). Rimz does not own unattended mode.

## Telemetry is opt-in

```sh
rimz hooks claude install --telemetry      # add high-frequency hooks
rimz hooks claude install --no-telemetry   # default; install lifecycle + feed only
```

Telemetry adds prompt-submit, pre-tool, and post-tool hooks that fire on every tool call. They're useful for **Recent activity** depth and post-hoc audit, but they carry tool inputs, prompts, file paths, and outputs into the ledger. Gate them against `[privacy] payload_mode`:

- `payload_mode = "metadata"` — strips inputs, prompts, args, errors. Smallest footprint.
- `payload_mode = "redacted"` — keeps bounded payloads with built-in redaction. Default.
- `payload_mode = "full"` — keeps hook payloads as delivered. `rimz doctor` warns.

## Later agents

OpenCode, Pi, Cursor, Gemini, Copilot, Amp, Rovo, Hermes, Factory, Qoder, and similar agents land through the same trait once their hook surfaces and decision outputs are verified.

Adding an agent requires tests for: install/uninstall, lifecycle mapping, feed classification, neutral stdout, decision stdout, PID attribution, version drift behaviour.

Pinned hook stdout shapes live as inline `insta::assert_*_snapshot!(... @"...")` goldens inside each adapter module — see [`crates/rimz/src/agents/claude.rs`](../../crates/rimz/src/agents/claude.rs) and [`crates/rimz/src/agents/codex.rs`](../../crates/rimz/src/agents/codex.rs). New adapters pin their shapes the same way.

## Appendix — Claude Code

Default install:

```text
SessionStart   SessionEnd   Stop   Notification
PermissionRequest
PreToolUse: ExitPlanMode
PreToolUse: AskUserQuestion
```

Telemetry install adds: `UserPromptSubmit`, `PreToolUse` (broad), `PostToolUse` (broad).

Decision shapes — Claude requires `hookSpecificOutput`:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PermissionRequest",
    "decision": { "behavior": "allow" }
  }
}
```

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow",
    "updatedInput": {}
  }
}
```

`ExitPlanMode` and `AskUserQuestion` require `updatedInput`. The Claude adapter sets `hook_cap = 120s` (Claude's upstream cap is ~125s; Rimz leaves a 5s safety margin so the bridge times out before the agent kills the hook). The exact value lives in `CLAUDE_HOOK_CAP` in `crates/rimz/src/agents/claude.rs`.

## Appendix — Codex

Default install:

```text
SessionStart   Stop   PermissionRequest
```

Telemetry install adds prompt submit and tool telemetry where supported.

Decision shape — Codex permission hooks emit only:

```json
{ "decision": "allow" }
```

```json
{ "decision": "deny" }
```

Never emit `updatedInput`, `updatedPermissions`, or `interrupt` for Codex permission hooks — those fields belong to other Codex hook types and corrupt the permission decision when included. Codex's hook cap is shorter than Claude's; chain budgets should account for it.

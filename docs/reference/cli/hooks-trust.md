# Hooks and trust

These commands wire agent hooks and grant project trust — the two edits RimZ makes to give itself a live view of your agents and to gate what a project may execute. Both are explicit and reversible: `hooks install` preserves user-owned config and previews its diff with `--dry-run`, `hooks uninstall` removes only RimZ's own blocks or whole-file integrations, and `trust grant`/`trust revoke` are one grant record on this machine. The safety model behind them is [security and trust](../../guide/security.md).

## Agent hooks

```sh
rimz hooks install [--dry-run] [AGENT]
rimz hooks uninstall [AGENT]
```

`hooks install` writes RimZ-managed hook entries into each agent's per-user configuration so the agent reports its lifecycle and blocking prompts back to RimZ. With no `AGENT` it installs every detected supported agent on PATH; with an explicit kind (`claude`, `codex`, `pi`, `grok`, …) it installs that agent. The command prints one per-agent summary with an `installed`, `refreshed`, or `hooks up to date` disposition, the hook count, every configuration-file artifact it touched, and a new-file or updated-config annotation. Structured installers merge RimZ's entries while preserving user hooks. Whole-file Pi and OpenCode integrations refresh a marked `_rimz_managed` file byte-for-byte from the running build and refuse an unmarked user file at the same path. `--dry-run` prints one existing/new-file row and unified diff per artifact to stdout and writes no files, so you can inspect or pipe every exact edit before it happens. Interactive setup detects an older marked whole-file integration, offers its refresh in the same summarized consent step as missing hooks, and points to this dry-run command for exact diffs. Antigravity's artifacts are `~/.gemini/config/hooks.json` and `~/.gemini/antigravity-cli/settings.json`.

Grok's structured installer owns `${GROK_HOME:-~/.grok}/hooks/rimz.json`, merges its `_rimz_managed` entries with unrelated global hooks, and sets a four-second command timeout. Its passive catalog excludes blocking `PreToolUse`; hook stdout stays empty and Grok's native UI remains the decision surface.

`hooks uninstall` removes only RimZ-managed hook blocks and restores wrapped user statuslines, leaving every other value untouched. It prints one per-agent summary with the removed hook count and every configuration file touched. Cursor's install is a two-file transaction across `~/.cursor/hooks.json` and `~/.cursor/cli-config.json`: it wraps the prior `statusLine` command, forwards that command by direct argv, rolls back the first write if the second fails, and restores the exact prior JSON value on uninstall. With no `AGENT` uninstall removes every installed set, prints a friendly nothing-to-uninstall notice when none are present, and exits successfully without needing the binary on PATH. This is the clean undo for `hooks install`.

Installed hooks call back into RimZ for lifecycle and blocking-ask events. Hook stdout is the agent's decision channel, so installed hooks keep diagnostics off stdout and return only the agent-native neutral no-op for blocking asks; the prompt stays in the agent UI ([the adapter boundary](../../internals/agents/adapter.md#the-hook-path)). Some agents add their own hook trust gate; when one reports installed-but-untrusted hooks, `rimz doctor` prints the exact fix.

## Project trust

```sh
rimz trust [status|grant|revoke] [--json]
```

`trust status` reads only. It re-hashes the project's executable surface and prints one of four states:

| State | Meaning |
| --- | --- |
| `no project config` | No `.rimz/config.toml` exists — the project has no executable surface |
| `untrusted` | Project config present, no grant record on this machine |
| `trusted` | Grant record present and the surface hash matches |
| `stale` | A command-running field changed since the grant; behaves like untrusted until the grant is refreshed |

`trust grant` pins the current hash and surface on this machine; `trust revoke` removes the grant, reverting the workspace to `untrusted`. Both `status` and `grant` render a field-level diff of what changed since the grant, so a refresh is informed. `--json` emits the state, ids, paths, hashes, grant timestamp, and the structured diff.

A fresh interactive `rimz start` on an untrusted project offers the same grant once; declining it remembers the current surface until `.rimz/config.toml` changes. Interactive `rimz loop fire` and project-task edits show the surface diff and offer the grant inline whenever their trust gate is closed.

Project trust covers project-supplied command surfaces: hook commands, agent launch commands, profile and team definitions, env overrides, and other executable fields. Until you grant trust, none of these run. The hash, stored surface, and record format are in [project trust](../../internals/harness/trust.md); the operator-facing safety model is in [security and trust](../../guide/security.md).

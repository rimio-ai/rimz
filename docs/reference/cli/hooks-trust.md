# Hooks and trust

These commands wire agent hooks and grant project trust — the two edits RimZ makes to give itself a live view of your agents and to gate what a project may execute. Both are explicit and reversible: `hooks install` is additive and previews its diff with `--dry-run`, `hooks uninstall` removes only RimZ's own blocks, and `trust grant`/`trust revoke` are one grant record on this machine. The safety model behind them is [security and trust](../../guide/security.md).

## Agent hooks

```sh
rimz hooks install [--dry-run] [AGENT]
rimz hooks uninstall [AGENT]
```

`hooks install` writes RimZ-managed hook entries into each agent's per-user configuration so the agent reports its lifecycle and blocking prompts back to RimZ. With no `AGENT` it installs every detected supported agent on PATH and prints a JSON array of reports; with an explicit kind (`claude`, `codex`, `pi`, …) it prints the single report. Each report contains every configuration-file artifact it touched. The install is additive — your existing hooks stay — and `--dry-run` prints one existing/new-file row and unified diff per artifact to stderr and writes no files, so you see every exact edit before it happens. Antigravity's artifacts are `~/.gemini/config/hooks.json` and `~/.gemini/antigravity-cli/settings.json`.

`hooks uninstall` removes only RimZ-managed hook blocks and restores wrapped user statuslines, leaving every other value untouched. Cursor's install is a two-file transaction across `~/.cursor/hooks.json` and `~/.cursor/cli-config.json`: it wraps the prior `statusLine` command, forwards that command by direct argv, rolls back the first write if the second fails, and restores the exact prior JSON value on uninstall. With no `AGENT` uninstall removes every installed set, prints `[]` when nothing is installed, and exits successfully without needing the binary on PATH. This is the clean undo for `hooks install`.

Installed hooks call back into RimZ for lifecycle and blocking-ask events. Hook stdout is the agent's decision channel, so installed hooks keep diagnostics off stdout and return only the agent-native neutral no-op for blocking asks; the prompt stays in the agent UI ([the adapter boundary](../../internals/agents/model.md#the-adapter-boundary)). Some agents add their own hook trust gate; when one reports installed-but-untrusted hooks, `rimz doctor` prints the exact fix.

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

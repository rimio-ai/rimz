# Hooks and trust

These commands wire agent hooks and grant project trust. Hooks give Rimz its live view of every agent, from lifecycle transitions to blocking prompts, and trust gates the command surfaces a project can supply. The safety model is [security and trust](../../guide/security.md).

## Agent hooks

```sh
rimz hooks install [--dry-run] [AGENT]
rimz hooks uninstall [AGENT]
```

`hooks install` writes Rimz-managed hook entries into the agent's per-user config. With no `AGENT` it installs every detected supported agent on PATH and prints a JSON array of reports; with an explicit kind (`claude`, `codex`, `pi`, …) it prints the single report. `--dry-run` prints the same per-agent summary plus a unified diff to stderr and writes no files.

`hooks uninstall` removes only Rimz-managed hook blocks. With no `AGENT` it removes every installed set, prints `[]` when nothing is installed, and exits successfully without needing the binary on PATH.

Installed hooks call Rimz's hidden hook entrypoint for lifecycle and blocking ask events. Hook stdout is the agent decision channel, so installed hooks keep diagnostics off stdout and return only the agent-native neutral no-op for blocking asks; the prompt stays in the agent UI ([the adapter boundary](../../internals/agents/model.md#the-adapter-boundary)). Some agents add their own hook trust gate; when one reports installed-but-untrusted hooks, `rimz doctor` prints the exact fix.

## Project trust

```sh
rimz trust [status|grant|revoke] [--json]
```

`trust status` (the default) re-hashes the project's executable surface and prints one of four states:

| State | Meaning |
| --- | --- |
| `no project config` | No `.rimz/config.toml` exists — the project has no executable surface |
| `untrusted` | Project config present, no grant record on this machine |
| `trusted` | Grant record present and the surface hash matches |
| `stale` | A command-running field changed since the grant; behaves like untrusted until the grant is refreshed |

`trust grant` pins the current hash and surface on this machine; `trust revoke` removes the grant. Both `status` and `grant` render a field-level diff of what changed since the grant, so a refresh is informed. `--json` emits the state, ids, paths, hashes, grant timestamp, and the structured diff.

A fresh interactive `rimz start` on an untrusted project offers the same grant once; declining it remembers the current surface until `.rimz/config.toml` changes.

Project trust covers project-supplied command surfaces: hook commands, agent launch commands, profile and team definitions, env overrides, and other executable fields. The hash, stored surface, and record format are in [project trust](../../internals/harness/trust.md); the operator-facing safety model is in [security and trust](../../guide/security.md).

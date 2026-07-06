# Feed, events, hooks, and trust

These commands publish items to the room's ledger, answer them, wire agent hooks, and grant project trust. The model behind them is [a programmable harness](../../../DESIGN.md#a-programmable-harness); the actionable surfaces are [the two feed surfaces](../../../DESIGN.md#the-two-feed-surfaces).

## Common script flows

Block a deployment gate until another process answers, then read the decision JSON:

```sh
decision=$(rimz feed ask --title "Promote build 2026.06.10-rc.4 to prod?" --options yes,no,abort --timeout 4h)
```

Post progress without blocking, or create a question and resolve it from another process:

```sh
rimz event emit --kind deploy.started --title "Deploy started" --json '{"env":"prod"}'
request_id=$(rimz feed ask --title "Continue migration?" --options yes,no --no-block)
rimz feed resolve --decision '{"choice":"yes"}' "$request_id"
```

Wire hooks and grant trust so agent prompts appear in the sidebar:

```sh
rimz hooks install claude
rimz trust grant
```

## Feed items and decisions

Every actionable feed item carries a `surface`, set when it is created. The surface decides which feed verb is meaningful:

| Surface | Source | Meaningful verb |
| --- | --- | --- |
| `native_ui` | agent hook | `rimz feed resolve` records a pane answer; `rimz feed dismiss` records local acknowledgement |
| `script` | `rimz feed ask` | `rimz feed resolve` answers the blocked script |

The verbs:

- **`ask`** creates a `script` item, prints the request id, and without `--no-block` waits for the decision JSON and prints it. `--options` supplies button labels; `--timeout` bounds the wait.
- **`push`** posts a non-blocking `native_ui` notice and prints the request id.
- **`list`** prints items newest first (`request_id`, `status`, `surface`, `title`); the runtime view hides records whose owner process is gone, and `--audit` reads durable history. **`show`** is always an exact audit lookup by id.
- **`resolve`** records a decision for a `native_ui` or `script` item: it stores the `--decision` JSON, clears the pending row when effective, wakes a waiting script when one exists, and prints `<request-id> effective=<bool> late=<bool>`. `--by <label>` attributes the action; `--method` records how it was answered (`pane-send`, `cli` (default), `sidebar`, or `workspace-reset`).
- **`dismiss`** acknowledges a `native_ui` item locally without answering the agent.

The ledger owns the socket, nonce, compare-and-swap, late-answer, and audit rules; the wire contract is in [ledger](../../internals/sidebar/ledger.md).

## Events

`event emit` appends a fire-and-forget workspace event and prints the event id. Unlike `feed push`, an event is a ledger record rather than a feed item — use it for structured progress that tooling reads, not for something a person needs to act on. `--kind` is a free-form tag (agent integrations prefer `<source>.<verb>`) and `--json` is stored as a structured payload.

```sh
rimz event emit --kind build.started --title web --json '{"sha":"abc123"}'
rimz event emit --kind deploy.finished --title prod --body "Canary passed."
```

## Resolver handlers

Resolvers are user-built handlers over public commands. Wire a notification handler for `waiting` rows, inspect with `feed show`, answer with pane primitives or `message`, and record with `feed resolve --by <name>`.

```toml
[[notifications.handler]]
when = { kind = ["waiting"] }
command = "python3 ~/bin/pane_send_resolver.py"
```

The pattern and examples are in [resolvers](../../internals/agents/resolvers.md), and the threat model is in [security](../../guide/security.md).

## Agent hooks

```sh
rimz hooks install [--dry-run] [AGENT]
rimz hooks uninstall [AGENT]
```

`hooks install` writes Rimz-managed hook entries into the agent's per-user config. With no `AGENT` it installs every detected supported agent on PATH and prints a JSON array of reports; with an explicit kind (`claude`, `codex`, `pi`, …) it prints the single report. `--dry-run` prints the same per-agent summary plus a unified diff to stderr and writes no files. `hooks uninstall` removes only Rimz-managed hook blocks — with no `AGENT` it removes every installed set, prints `[]` when nothing is installed, and exits successfully without needing the binary on PATH.

Installed hooks call Rimz's hidden hook entrypoint for lifecycle and blocking feed events. Hook stdout is the agent decision channel, so installed hooks keep diagnostics off stdout and return only the agent-native neutral no-op for blocking asks; the prompt stays in the agent UI ([the adapter boundary](../../internals/agents/agent.md#the-adapter-boundary)). Some agents add their own hook trust gate; when one reports installed-but-untrusted hooks, `rimz doctor` prints the exact fix.

## Project trust

```sh
rimz trust [status|grant|revoke] [--json]
```

`trust status` (the default) re-hashes the project's executable surface and prints `no project config`, `untrusted`, `trusted`, or `stale`. `trust grant` pins the current hash on this machine; `trust revoke` removes the grant. A later edit to a command-running project field makes the state `stale`, which behaves like untrusted until the grant is refreshed. `--json` emits the state, ids, paths, hashes, and grant timestamp.

Project trust covers project-supplied command surfaces — hook commands, agent launch commands, layout commands, and other executable fields. The hash and record format are in [project trust](../../internals/sidebar/trust.md); the operator-facing safety model is in [security and trust](../../guide/security.md).

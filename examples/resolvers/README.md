# Reference resolvers

Reference resolver artifacts for tests and documentation. **Not shipped as
product.** They exist to prove the resolver protocol from
[`docs/internals/resolvers.md`](../../docs/internals/resolvers.md) is
implementable from outside the workspace, using only the public CLI.

Both scripts are single-file Python 3 (stdlib only — no third-party deps).
They write their own heartbeats and shell out to `rimz feed list / show /
resolve / abstain` plus, for the pane variant, `rimz pane capture / send`.

## `hook_bridge_resolver.py`

Demonstrates the **fast path** for agents with rich permission hooks: return
a decision JSON while the agent's hook is on the bridge.

The built-in policy is intentionally minimal — `allow` when the agent
reports a `tool_name` in `{Read, Grep, Glob, LS}`, `abstain` otherwise.
Real resolvers wrap a model or an organization policy here.

```sh
# In one terminal: enrol the resolver and run it.
rimz resolver add demo --order 10 --budget 30s
python3 examples/resolvers/hook_bridge_resolver.py \
    --workspace-id "$(rimz workspace resolve --json | jq -r .workspace_id)" \
    --resolver-id demo \
    --display-name "Demo policy"

# In another terminal: fire a permission request via the agent. The resolver
# auto-answers (or abstains) within ~1s. The audit trail is in
# `rimz feed show <request-id>`.
```

## `pane_send_resolver.py`

Demonstrates the **universal answer surface** — capturing a TTY pane,
matching the captured text against a bounded regex list, typing an answer
through `rimz pane send`, re-capturing to confirm, and finally calling
`rimz feed resolve --method pane-send` so the ledger reflects what
happened.

```sh
rimz resolver add pane-demo --order 20 --budget 1m
python3 examples/resolvers/pane_send_resolver.py \
    --workspace-id "$(rimz workspace resolve --json | jq -r .workspace_id)" \
    --resolver-id pane-demo \
    --display-name "Pane demo"
```

The bounded regex list matches a handful of well-known prompts (`Are you
sure? [y/N]`, `Do you want to continue? [y/N]`, `Proceed? [Y/n]`). Anything
else abstains. **Pane text is untrusted data**; if you extend this resolver
to ask a model "what should I type?", you are vulnerable to prompt
injection from anything that can print to a pane.

## Discipline

Both scripts follow the resolver discipline documented in
[`docs/internals/resolvers.md`](../../docs/internals/resolvers.md):

- **Heartbeat is a file the resolver owns.** Atomic temp-file + rename so
  partial writes don't leak.
- **CAS failures are normal.** Another link in the chain or a human may
  resolve before us; non-zero exits from `rimz feed resolve` are logged at
  stderr and the resolver moves on.
- **Pane primitives belong to resolvers, not core.** Core never types into a
  pane on the user's behalf; that's why this script exists.
- **Bounded patterns only.** The pane resolver matches only the strings it
  recognises. Real deployments narrow this further; never widen it past
  what you understand.
- **Clean SIGTERM.** Removing the heartbeat on exit so the chain advances
  immediately when the resolver is killed.

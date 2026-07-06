# Reference resolvers

Reference resolver artifacts for tests and documentation. **Not shipped as product.** They prove the resolver pattern from [`docs/internals/agents/resolvers.md`](../../docs/internals/agents/resolvers.md) is implementable from outside the workspace with public Rimz commands.

Resolvers are notification handlers: the sidebar producer invokes a command for `waiting` rows, and that command decides whether it can answer in the agent's own pane. Unknown shapes are answered by silence, so the prompt remains for you.

## `pane_send_resolver.py`

Captures the pane named by `RIMZ_NOTIFY_PANE`, matches the captured text against a bounded regex list, types an answer through `rimz pane send`, re-captures to confirm, and records the outcome with `rimz feed resolve --method pane-send --by pane-send-resolver`.

```toml
[[notifications.handler]]
when = { kind = ["waiting"] }
command = "python3 /path/to/examples/resolvers/pane_send_resolver.py"
```

The bounded regex list matches a handful of well-known prompts (`Are you sure? [y/N]`, `Do you want to continue? [y/N]`, `Proceed? [Y/n]`). **Pane text is untrusted data**; if you extend this resolver to ask a model what to type, treat captured text as data, not instructions.

## `agent_resolver.sh`

Delegates the same item to a supervised agent run. The spawned agent receives the request id, pane id, workspace root, and feed JSON, then answers with the same pane-send + feed-resolve contract when policy applies.

```toml
[[notifications.handler]]
when = { kind = ["waiting"] }
command = "/path/to/examples/resolvers/agent_resolver.sh"
```

Set `RIMZ_RESOLVER_AGENT_KIND=claude` or another registered kind to choose the answering agent.

## Discipline

- **Watch with notifications.** Handlers are per-machine config and run only when you wire them.
- **Inspect through public APIs.** Use `rimz feed show --json`, `rimz pane capture`, `rimz agents list`, and transcripts.
- **Answer in the agent's UI.** Use `rimz pane send` or `rimz message`; the original prompt remains visible.
- **Record the outcome.** Use `rimz feed resolve <request-id> --method pane-send --by <name>` so the sidebar clears and the audit trail says who acted.
- **Stay bounded.** Match known prompts, re-capture after sending, and do nothing on unknown text.

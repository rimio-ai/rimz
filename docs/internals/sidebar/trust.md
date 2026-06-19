# Project trust

> See [`docs/guide/security.md`](../../guide/security.md) for the user-facing threat model and [`docs/reference/cli.md`](../../reference/cli.md) for the `rimz trust` surface.

Project config at `<project_root>/.rimz/config.toml` is inert until the workspace is trusted. A grant pins a SHA-256 of every command-running field; later reads re-hash and demote to stale when the hash drifts.

## States

| State | Meaning |
| --- | --- |
| `no_config` | No `.rimz/config.toml` exists. Nothing to trust. |
| `untrusted` | Config present, no grant record on this machine. Command-running fields are inert. |
| `trusted` | Grant record matches the current executable-surface hash. |
| `stale` | Grant record exists but the surface hash drifted since the grant. Equivalent to `untrusted` for any command-running gate. |

`stale` is the auto-revoke half of the contract: `rimz trust status` recomputes the hash every call, so there is no separate sweep — the next read of the workspace sees the new state.

## Executable surface

Every field that can cause a process to run enters the hash. The current projection lives in [`crates/rimz/src/trust.rs::ExecutableSurface`](../../../crates/rimz/src/trust.rs).

- `[[layout.initial_panes]]` — `name`, `command`, `cwd`, `env`.
- `[layout.tmux]` — `status_left`, `status_right`, `popup_command`.
- `[[agents]]` — `name`, `launch_command`, `env`.
- `[agents.teams.<name>]` — `layout` plus each role's `role`, `profile`, `mode`, `model`, `effort`, `system-prompt-file`, `args`.
- `[profiles.<name>]` — `agent`, `mode`, `model`, `effort`, `system-prompt-file`, `args`.
- `[[hooks]]` — `event`, `command`.
- `[env]` — every key/value (PATH-affecting overrides included).

The hash input is canonical JSON over `ExecutableSurface`. Struct field order is fixed, `BTreeMap` keys sort, `Option::None` serializes as `null`. The wire format is `sha256:<hex>`.

Per-machine commands such as `[notifications].command` in `~/.config/rimz/config.toml` and per-machine `[agents.profiles]` / `[agents.teams]` in `~/.config/rimz/agents.toml` are outside this hash. They are personal machine policy, not cloned project policy. Repo `[profiles]` and `[agents.teams]` are hash-covered and inert until trusted; repo profiles may inherit only repo profiles or built-in kinds, and repo team roles bind repo profiles so the shared launch shape stays closed over the hashed config.

## Launch-time application

`[[agents]]` `env`, top-level `[profiles]`, and `[agents.teams]` are the applied surfaces: on a `trusted` workspace, the `rimz agents exec` wrapper injects each matching env entry into the agent process it spawns, and the `rimz agents` resolver overlays repo profiles and teams over machine config. Project config uses one `agents` shape at a time: `[[agents]]` for env entries or `[agents.teams]` for shared teams. Every agent launcher (`rimz agents`, supervised `-p`, and [resume-on-rebirth seeds](sidebar.md#resume-on-rebirth)) funnels through that wrapper. Entries sharing a name merge in declaration order, later entries win on key collisions, and values pass literally. The wrapper launches through the user's default shell startup path when that shell is launchable, then execs `/usr/bin/env` to re-apply Rimz's launch env after shell rc/profile files have run. Effective precedence from lowest to highest is pane env, shell rc/profile env, trusted project `[[agents]]` env, adapter launch built-ins, then `RIMZ_RUN_ID` / `RIMZ_AGENT_PROFILE` / `RIMZ_AGENT_ROLE`; rc files are per-machine personal policy outside the project trust hash. On an `untrusted` or `stale` workspace a configured agent env, repo profile, or repo team refuses the launch with the `rimz trust grant` fix, and malformed launch env keys refuse before any tab, worktree, or run-record side effect. The remaining executable-surface fields are hash-only today; their planned application is tracked in [reference/configuration.md](../../reference/configuration.md#project-config).

Adapter launch built-ins ([`AgentAdapter::launch_env`](../../../crates/rimz/src/agents/mod.rs), e.g. the Claude classic-REPL pin in [claude-reference.md → Agent view](../../externals/agent-adapter/claude-reference.md#agent-view)) apply after the project env: a trusted config tunes an agent's launch, and the integration's own launch contract stays pinned.

Adding a command-running field that isn't projected into `ExecutableSurface` is a CI invariant violation — the `hash_covers_every_documented_surface_field` unit test will collide and the `docs/guide/security.md` doc gate will not match.

## Storage

- **Project config.** `<project_root>/.rimz/config.toml`. Committed.
- **Trust record.** `$XDG_CONFIG_HOME/rimz/projects/<workspace_id>/trust.toml`. Per-machine. Atomic temp+rename writes through [`ledger::atomic::write_bytes_atomically`](../../../crates/rimz/src/ledger/atomic.rs).

Record schema:

```toml
project_root = "/home/me/code/query-engine"
surface_hash = "sha256:..."
granted_at   = "2026-05-23T12:34:56Z"
```

## CLI

```sh
rimz trust [status|grant|revoke] [--json]
```

`status` is the default. `grant` pins the live hash; `revoke` deletes the record. `rimz doctor` surfaces the trust state alongside the protocol and resolver checks.

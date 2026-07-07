# Project trust

> See [`docs/guide/security.md`](../../guide/security.md) for the user-facing threat model and [`docs/reference/cli.md`](../../reference/cli.md) for the `rimz trust` surface.

Trust is the harness permission model: the gate on the repo-shipped launch surface — agents, profiles, teams, hooks, env — that a clone brings onto a machine. Project config at `<project_root>/.rimz/config.toml` is inert until the workspace is trusted. A grant pins a SHA-256 of every command-running field and stores the granted surface itself; later reads re-hash, demote to stale when the hash drifts, and render a field-level diff of what changed since the grant.

The hash pin is deliberate where a folder-wide grant would not be: agents in the room can write `.rimz/config.toml` themselves, and the pin keeps an agent-authored (or pulled-in) config edit from granting itself command execution at the next launch.

## States

| State | Meaning |
| --- | --- |
| `no_config` | No `.rimz/config.toml` exists. Nothing to trust. |
| `untrusted` | Config present, no grant record on this machine. Command-running fields are inert. |
| `trusted` | Grant record matches the current executable-surface hash. |
| `stale` | Grant record exists but the surface hash drifted since the grant. Equivalent to `untrusted` for any command-running gate; the report carries the [surface diff](#the-surface-diff). |

`stale` is the auto-revoke half of the contract: `rimz trust status` recomputes the hash every call, so there is no separate sweep — the next read of the workspace sees the new state.

## Executable surface

Every field that can cause a process to run enters the hash. The current projection lives in [`crates/rimz/src/trust.rs::ExecutableSurface`](../../../crates/rimz/src/trust.rs).

- `[[agents]]` — `name`, `launch_command`, `env`.
- `[agents.teams.<name>]` — `layout` plus each role's `role`, `profile`, `mode`, `model`, `effort`, `system-prompt-file`, `append-system-prompt-file`, `args`.
- `[profiles.<name>]` — `agent`, `mode`, `model`, `effort`, `system-prompt-file`, `append-system-prompt-file`, `args`.
- `[tasks.<name>]` — `spec`, `prompt`, `prompt-file`, `check`, `on`, `worktree`, `mode`, `effort`, `system-prompt-file`, `timeout`, `at`, `at-reset`, `days`, `every`, `cron`, `once`.
- `[[hooks]]` — `event`, `command`.
- `[env]` — every key/value (PATH-affecting overrides included).

The hash input is canonical JSON over `ExecutableSurface`. Struct field order is fixed, `BTreeMap` keys sort, `Option::None` serializes as `null`. The wire format is `sha256:<hex>`.

Room layout is per-machine policy, not project policy: a project config carrying a `[layout]` table (including `[[layout.initial_panes]]` and `[layout.tmux]`) fails the read with the fix — move it to `$XDG_CONFIG_HOME/rimz/config.toml` ([`check_project_config_removed_tables`](../../../crates/rimz/src/trust.rs)).

Per-machine commands such as `[[notifications.handler]]` and `[notifications].command` in `~/.config/rimz/config.toml`, per-machine `[agents.profiles]` / `[agents.teams]` in `~/.config/rimz/agents.toml`, and per-machine loop `check` commands in `~/.config/rimz/loop.toml` are outside this hash. They are personal machine policy, not cloned project policy. Repo `[profiles]`, `[agents.teams]`, and `[tasks]` are hash-covered and inert until trusted; repo profiles may inherit only repo profiles or built-in kinds, repo team roles bind repo profiles so the shared launch shape stays closed over the hashed config, and repo tasks run only at the project root.

## Launch-time application

`[[agents]]` `env`, top-level `[profiles]`, `[agents.teams]`, and `[tasks]` are the applied surfaces: on a `trusted` workspace, the `rimz agents exec` wrapper injects each matching env entry into the agent process it spawns, the `rimz agents` resolver overlays repo profiles and teams over machine config, and `rimz loop` overlays repo tasks over machine tasks and state instances. Project config uses one `agents` shape at a time: `[[agents]]` for env entries or `[agents.teams]` for shared teams. Every agent launcher (`rimz agents`, supervised `-p`, and [resume-on-rebirth seeds](../sidebar/sidebar.md#resume-on-rebirth)) funnels through that wrapper. Entries sharing a name merge in declaration order, later entries win on key collisions, and values pass literally. The wrapper launches through the user's default shell startup path when that shell is launchable, then execs `/usr/bin/env` to re-apply Rimz's launch env after shell rc/profile files have run. Effective precedence from lowest to highest is pane env, shell rc/profile env, trusted project `[[agents]]` env, adapter launch built-ins, then `RIMZ_RUN_ID` / `RIMZ_AGENT_PROFILE` / `RIMZ_AGENT_ROLE` / `RIMZ_AGENT_MODEL` / `RIMZ_AGENT_EFFORT`; rc files are per-machine personal policy outside the project trust hash. On an `untrusted` or `stale` workspace a configured agent env, repo profile, repo team, or project-only task refuses execution with the `rimz trust grant` fix, while same-named machine loop tasks keep running; malformed launch env keys refuse before any tab, worktree, or run-record side effect. The remaining executable-surface fields are hash-only today; their planned application is tracked in [reference/configuration.md](../../reference/configuration.md#project-config).

Adapter launch built-ins ([`AgentAdapter::launch_env`](../../../crates/rimz/src/agents/mod.rs), e.g. the Claude classic-REPL pin in [claude-reference.md → Agent view](../../externals/agent-adapter/claude-reference.md#agent-view)) apply after the project env: a trusted config tunes an agent's launch, and the integration's own launch contract stays pinned.

Adding a command-running field that isn't projected into `ExecutableSurface` is a CI invariant violation — the `hash_covers_every_documented_surface_field` unit test will collide and the `docs/guide/security.md` doc gate will not match.

## Storage

- **Project config.** `<project_root>/.rimz/config.toml`. Committed.
- **Trust record.** `$XDG_CONFIG_HOME/rimz/projects/<workspace_id>/trust.toml`. Per-machine. Atomic temp+rename writes through [`store::atomic::write_bytes_atomically`](../../../crates/rimz/src/store/atomic.rs).

Record schema:

```toml
project_root = "/home/me/code/query-engine"
surface_hash = "sha256:..."
surface_json = '{"agents":[...],"profiles":[...],...}'
granted_at   = "2026-05-23T12:34:56Z"
```

`surface_json` is the canonical surface JSON the hash was computed over, kept so a stale grant can say *what* drifted, not just that it did.

## The surface diff

A `stale` report carries a field-level diff of granted vs current surface: a structured walk over the two canonical JSON values yielding added, removed, and changed leaves with their paths ([`trust.rs::executable_surface_diff`](../../../crates/rimz/src/trust.rs)). `rimz trust status` renders it under the state line; `rimz trust grant` renders it before pinning the new surface, so a re-grant is informed rather than blind; `--json` carries the entries as the `surface_diff` array.

## CLI

```sh
rimz trust [status|grant|revoke] [--json]
```

`status` is the default. `grant` renders the surface diff when a prior record exists, then pins the live hash and surface; `revoke` deletes the record. `rimz doctor` surfaces the trust state alongside the protocol checks.

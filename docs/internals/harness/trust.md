# Project trust

> [security.md](../../guide/security.md) owns the operator threat model, [configuration.md § Project config](../../guide/configuration.md#project-config) owns the config surface a repo ships, and the [`rimz trust` reference](../../reference/cli/hooks-trust.md#project-trust) owns the command. This doc owns the mechanics: the executable-surface hash, the grant record, launch-time enforcement, and the stale-grant diff.

Trust is the gate on the launch surface a clone brings onto a machine. Project config at `<project_root>/.rimz/config.toml` can name agents, profiles, teams, loop tasks, hooks, and env, and any of those can run a command, so the whole file is inert until the workspace is trusted. A grant pins a SHA-256 over every command-running field and stores the granted surface alongside it. Every later read re-hashes the live config, demotes the state to `stale` when the hash drifts, and renders a field-level diff of what changed since the grant.

The pin is per-surface, not per-folder, because agents in the room can write `.rimz/config.toml` themselves. Hashing the exact executable surface keeps an agent-authored or pulled-in config edit from granting itself command execution at the next launch: any change to a command-running field breaks the hash and reverts the workspace to `stale` until a human re-grants.

## States

The state is derived on every read from the live hash and the on-disk record ([`TrustState`](../../../crates/rimz/src/trust.rs)); it is never written as state directly.

| State | Meaning |
| --- | --- |
| `no_config` | No `.rimz/config.toml` exists. The project has no executable surface. |
| `untrusted` | Config present, no grant record on this machine. Command-running fields are inert. |
| `trusted` | Grant record present and its hash matches the current surface. |
| `stale` | Grant record present but the surface hash drifted since the grant. Treated as `untrusted` for every command-running gate; the report carries the [surface diff](#the-surface-diff). |

`stale` is the auto-revoke half of the contract. [`status`](../../../crates/rimz/src/trust.rs) recomputes the hash on every call, so a drifted surface surfaces as `stale` on the next read of the workspace, with no separate sweep. `rimz trust status` and `rimz doctor` both re-hash live.

## The executable surface

Every field that can cause a process to run enters the hash. The projection is [`ExecutableSurface`](../../../crates/rimz/src/trust.rs), and each entry below is one of its fields:

- `[[agents]]` — `name`, `launch_command`, `env`.
- `[profiles.<name>]` — `agent`, `mode`, `model`, `effort`, `system-prompt-file`, `append-system-prompt-file`, `args`.
- `[agents.teams.<name>]` — `layout`, plus each role's `role`, `profile`, `mode`, `model`, `effort`, `system-prompt-file`, `append-system-prompt-file`, `args`.
- `[tasks.<name>]` — `spec`, `prompt`, `prompt-file`, `check`, `on`, `worktree`, `mode`, `effort`, `system-prompt-file`, `timeout`, `at`, `at-reset`, `days`, `every`, `cron`, `once`.
- `[[hooks]]` — `event`, `command`.
- `[env]` — every key and value.

The hash input is canonical JSON over `ExecutableSurface`: struct field order is fixed, `BTreeMap` keys sort, and `Option::None` serializes as `null`, so the same config always hashes to the same bytes. The wire format is `sha256:<hex>`. Non-command fields such as `display_name` or `sidebar_width` deserialize leniently and never touch the hash.

Room layout is per-machine policy, so a project config carrying a `[layout]` table (including `[[layout.initial_panes]]` and `[layout.tmux]`) fails the read with the fix to move it to `$XDG_CONFIG_HOME/rimz/config.toml` ([`check_project_config_removed_tables`](../../../crates/rimz/src/trust.rs)). Personal machine policy stays out of the hash entirely: per-machine `[[notifications.handler]]` and `[notifications].command`, per-machine `[agents.profiles]` and `[agents.teams]`, and per-machine loop `check` commands all live under `~/.config/rimz/` and are never trust-tracked.

To keep the hashed surface closed and machine-independent, repo profiles may inherit only repo profiles or built-in kinds, repo team roles bind only repo profiles, and repo tasks run only at the project root.

## Launch-time enforcement

Every agent launch funnels through the hidden `rimz agents exec` wrapper ([`exec.rs`](../../../crates/rimz/src/cli/agents_cmd/exec.rs)), which resolves the trust-gated env before it spawns the agent. That covers `rimz agents`, supervised `-p` runs, and [resume-on-rebirth seeds](../sidebar/sidebar.md#resume-on-rebirth). Loop tasks resolve through the same trust gate before firing.

Four surfaces apply today; the rest are hashed but not yet consumed at launch.

| Surface | On a `trusted` workspace | On `untrusted` / `stale` |
| --- | --- | --- |
| `[[agents]]` `env` | injected into the agent process | launch refuses with the `rimz trust grant` fix |
| `[profiles]` | overlaid over machine profiles, winning name collisions | a spec that references a repo profile refuses |
| `[agents.teams]` | overlaid over machine teams | a spec that references a repo team refuses |
| `[tasks]` | overlaid over machine loop tasks and state instances | the project task stays inert; a same-named machine task keeps running |
| `[[agents]]` `launch_command`, `[[hooks]]`, top-level `[env]` | hashed only | hashed only |

Applying the declared hooks, agent launch command, and top-level env is planned project-config behavior; those fields are covered by the hash today so that turning them on later needs no re-grant.

Project config uses one `agents` shape at a time: `[[agents]]` for env entries, or `[agents.teams]` for shared teams. [`agent_env`](../../../crates/rimz/src/trust.rs) resolves the `[[agents]]` env for one kind under the trust gate: entries sharing a name merge in declaration order, later entries win key collisions, and values pass literally with no shell expansion. It returns `Blocked` on an untrusted or stale workspace, and the launcher fails at that entry point with the grant fix ([`agent_launch_env`](../../../crates/rimz/src/cli/agents_cmd/launch.rs)). The profile, team, and task overlays live in [`config::effective`](../../../crates/rimz/src/config/effective.rs); `block_untrusted_reference` refuses only a launch spec that would actually consume a repo profile or team, so machine profiles and built-in kinds keep launching in an untrusted checkout that merely declares project config.

### Env application

An applied `[[agents]]` env reaches the agent through the login-shell wrapper ([`login_shell_argv`](../../../crates/rimz/src/harness/launch.rs)): the wrapper runs the user's default shell startup path when that shell is launchable, then execs `/usr/bin/env` to re-apply Rimz's launch env after the shell rc and profile files have run. Effective precedence, lowest to highest:

1. pane env
2. shell rc and profile env
3. trusted project `[[agents]]` env
4. adapter launch built-ins ([`AgentAdapter::launch_env`](../../../crates/rimz/src/agents/mod.rs))
5. `RIMZ_RUN_ID`, `RIMZ_AGENT_PROFILE`, `RIMZ_AGENT_ROLE`, `RIMZ_AGENT_MODEL`, `RIMZ_AGENT_EFFORT`, and the render-toolkit mode

Adapter built-ins apply after the project env so a trusted config tunes an agent's launch while the integration's own launch contract stays pinned. A malformed launch env key refuses before any tab, worktree, or run-record side effect ([`validate_agent_launch_env`](../../../crates/rimz/src/cli/agents_cmd/launch.rs)): keys must be non-empty, free of `=`, and not start with `-`.

## Storage

The committed **project config** is `<project_root>/.rimz/config.toml`. The **trust record** is per-machine, at `$XDG_CONFIG_HOME/rimz/projects/<workspace_id>/trust.toml`, written with atomic temp-plus-rename through [`store::atomic::write_bytes_atomically`](../../../crates/rimz/src/store/atomic.rs). Its schema:

```toml
project_root = "/home/me/code/query-engine"
surface_hash = "sha256:..."
surface_json = '{"agents":[...],"profiles":[...],...}'
granted_at   = "2026-05-23T12:34:56Z"
```

`surface_json` is the canonical surface JSON the hash was computed over. Storing it lets a stale grant report *what* drifted, not just that it did. A record missing `surface_json` fails to parse rather than silently degrading to a hash-only comparison.

## The surface diff

A `stale` report carries a field-level diff of the granted surface against the current one: a structured walk over the two canonical JSON values that yields added, removed, and changed leaves with their paths ([`executable_surface_diff`](../../../crates/rimz/src/trust.rs)). `rimz trust status` renders it under the state line, and `rimz trust grant` renders it before pinning the new surface so a re-grant is informed rather than blind. Under `--json` the entries ride in the `surface_diff` array.

## CLI

```sh
rimz trust [status|grant|revoke] [--json]
```

`status` is the default. `grant` renders the surface diff when a prior record exists, then pins the live hash and surface. `revoke` deletes the record, reverting the workspace to `untrusted` (or `no_config` when `.rimz/config.toml` is absent). `rimz doctor` surfaces the trust state alongside its protocol checks. The full command surface is in the [reference](../../reference/cli/hooks-trust.md#project-trust).

## Adding a command-running field

A new field that can run a process must be projected into `ExecutableSurface`. Two guards enforce this together: the `hash_covers_every_documented_surface_field` unit test hashes one config per field and fails if any two collide, and the `docs/guide/security.md` doc gate fails if the field is absent from the operator surface list. Land the projection, the test case, and the doc update in the same change.

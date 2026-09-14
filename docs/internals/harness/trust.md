# Project trust

> How RimZ decides whether a repository's `.rimz/config.toml` may run commands on this machine: the executable-surface hash, the grant record, launch-time enforcement, and the stale-grant diff. The code is `crates/rimz/src/trust.rs`, and [fleet.md](./fleet.md) is the map for this area. For users, [security.md](../../guide/security.md) owns the threat model, [configuration.md § Project config](../../guide/configuration.md#project-config) the config surface a repo ships, and the [`rimz trust` reference](../../reference/cli/hooks-trust.md#project-trust) the command.

Project config is inert until a human trusts it on this machine. A clone can ship agents, profiles, teams, loop tasks, hooks, and env in `<project_root>/.rimz/config.toml`, and each of those can run a command. A grant pins a SHA-256 over every command-running field and stores the granted surface beside it. Every later read re-hashes the live config; when the hash differs, the workspace is `stale` and the grant stops applying until someone re-grants.

The pin is on the surface itself because agents in the room can write `.rimz/config.toml` themselves. An agent-authored edit or a pulled change to any command-running field breaks the hash, so neither can grant itself command execution at the next launch.

## States

[`TrustState`](../../../crates/rimz/src/trust.rs) is derived on every read from the live hash and the on-disk grant record; it is never stored.

| State | Condition | Effect |
| --- | --- | --- |
| `no_config` | No `.rimz/config.toml`. | Nothing to gate. |
| `untrusted` | Config present, no grant record on this machine. | Every command-running field is inert. |
| `trusted` | Grant record present and its hash matches the live surface. | Trusted surfaces apply at launch. |
| `stale` | Grant record present, hash differs. | Same as `untrusted`; the report carries the [surface diff](#the-surface-diff). |

[`status`](../../../crates/rimz/src/trust.rs) re-hashes on every call, so `stale` is the auto-revoke: drift shows on the next read of the workspace with no background sweep. `rimz trust status`, `rimz doctor`, and every launch gate call it. [`blocked_fix`](../../../crates/rimz/src/trust.rs) is the fix text every trust-gated refusal prints, with a `stale` variant that names the surface change.

## The executable surface

[`ExecutableSurface`](../../../crates/rimz/src/trust.rs) projects the fields that can cause a process to run, and only those enter the hash.

| Config table | Hashed keys |
| --- | --- |
| `[[agents]]` | `name`, `launch_command`, `env` |
| `[profiles.<name>]` | `agent`, `skills`, `mode`, `model`, `effort`, `auto-compact`, `system-prompt-file`, `append-system-prompt-files`, `args` |
| `[subagents.profiles.<name>]` | the same keys as `[profiles.<name>]`, in the child-launch namespace |
| `[agents.teams.<name>]` | `layout`; per role: `role`, `profile`, `signals`, `mode`, `model`, `effort`, `auto-compact`, `system-prompt-file`, `append-system-prompt-files`, `args` |
| `[tasks.<name>]` | `agent`, `prompt`, `prompt-file`, `check`, `verify`, `max-attempts`, `on`, `worktree`, `mode`, `effort`, `system-prompt-file`, `timeout`, `at`, `every`, `cron`, `signal`, `match` |
| `[[hooks]]` | `event`, `command` |
| `[env]` | every key and value |
| `[accounts]` | every `<kind> = "<name>"` selection, because it redirects every agent's credentials |

The hash input is canonical JSON, and the wire format is `sha256:<hex>`. Struct field order is fixed, `BTreeMap` keys sort, and an unset `Option` serializes as `null`, so the same config always hashes to the same bytes. A few fields are omitted instead of written empty, so that grants made before the field existed keep their hash: empty `subagent_profiles`, empty `accounts`, empty role `signals`, and unset `skills` and `auto_compact` on profiles and roles. `append-system-prompt-files` serializes under the key `append_system_prompt_file` for the same reason.

Everything else in the file deserializes leniently and never touches the hash. That covers display keys such as `display_name` and `sidebar_width`, team `leader`, `owns`, `flip-compact`, `scratch-files`, and `stages`, and task `team`, `max-strikes`, `budget`, `budget-per-day`, `surplus`, and `surplus-after`.

Some tables are refused outright. [`check_project_config_removed_tables`](../../../crates/rimz/src/trust.rs) fails the read when the project config carries a `[layout]` table (which includes `[[layout.initial_panes]]` and `[layout.tmux]`), with the fix to move it to `$XDG_CONFIG_HOME/rimz/config.toml`, or a retired key that [`retired_agents_key`](../../../crates/rimz/src/config/agents.rs) names with its replacement. Project task fields that describe machine state fail to load; [loops.md § Where tasks live](./loops.md#where-tasks-live) owns that list.

Machine policy stays outside the hash because a repository cannot set it. Per-machine `[[notifications.handler]]` and `[notifications].command`, per-machine profiles, subagent profiles, and teams, per-machine loop `check` commands, and `agents.isolation` all live under `$XDG_CONFIG_HOME/rimz/` and are never trust-tracked. A repository therefore cannot choose host or sandbox isolation, and the [sandbox mount view](../sandbox.md) is no substitute for trust: profile skill views do not make untrusted commands safe.

The hashed surface is closed. A repo profile may inherit only repo profiles or built-in kinds (`RepoProfileEscapesTrust` otherwise), a repo team role binds only repo profiles, and a repo task always runs at the project root. A launch described by the project config therefore runs only hashed definitions, in a directory the project controls.

## Launch-time enforcement

Every agent launch compiles its process in the hidden `rimz agents exec` wrapper ([`exec.rs`](../../../crates/rimz/src/cli/agents_cmd/exec.rs)), which resolves the trust-gated env before it spawns the agent. That covers `rimz agents`, supervised `-p` runs, and [resume-on-rebirth seeds](../sidebar/sidebar.md#resume-on-rebirth). Loop tasks pass the same gate before firing.

| Surface | `trusted` | `untrusted` or `stale` |
| --- | --- | --- |
| `[[agents]]` `env` | injected into the agent process | launch refuses with the grant fix |
| `[profiles]` | overlaid on machine profiles, winning name collisions | a spec that uses a repo profile refuses |
| `[subagents.profiles]` | overlaid on machine subagent profiles, winning name collisions | a child spec that uses a repo subagent profile refuses |
| `[agents.teams]` | overlaid on machine teams | a spec that uses a repo team refuses |
| `[accounts]` | the default account selection a fresh room births with | `rimz start` refuses room birth with the grant fix |
| `[tasks]` | overlaid on machine loop tasks and state instances, then gated by machine-local enablement | the project task is filtered out; a same-named machine task keeps running |
| `[[agents]]` `launch_command`, `[[hooks]]`, top-level `[env]` | hashed, not consumed | hashed, not consumed |

The last row is hashed ahead of use, so that consuming those fields at launch will need no re-grant.

[`agent_env`](../../../crates/rimz/src/trust.rs) resolves the `[[agents]]` env for one kind. Entries sharing a name merge in declaration order, later entries win key collisions, and values pass literally with no shell expansion. On an untrusted or stale workspace it returns `AgentEnv::Blocked`, and `trusted_agent_env` in [`launch.rs`](../../../crates/rimz/src/harness/launch.rs) turns that into a `BlockedEnv` compile error, so the launch fails before any pane opens. Project config uses one `agents` shape at a time: the `[[agents]]` array for env entries, or the `[agents]` table for `[agents.teams]`.

The profile, team, and task overlays live in [`config::effective`](../../../crates/rimz/src/config/effective.rs). On a closed gate, `LaunchAgents::block_untrusted_reference` refuses only a spec that would actually consume a repo profile or team, so machine profiles and built-in kinds keep launching in an untrusted checkout that merely declares project config.

A trusted project task still needs its own arming: trust approves the config contents, and `rimz loop enable <name>` approves that task for unattended execution on this machine. [loops.md § Where tasks live](./loops.md#where-tasks-live) owns that second approval.

### Env application

[`compose_agent_env`](../../../crates/rimz/src/harness/launch.rs) layers the launch env, and the login-shell wrapper ([`login_shell_argv`](../../../crates/rimz/src/harness/launch.rs)) delivers it: the wrapper runs the user's shell startup files, then execs `/usr/bin/env` with RimZ's launch env as `KEY=VALUE` arguments, so the launch env wins over anything the rc files set. Precedence, lowest to highest:

1. pane env
2. shell rc and profile env
3. trusted project `[[agents]]` env
4. adapter launch built-ins ([`AgentDefinition::launch_env`](../../../crates/rimz/src/agents/mod.rs))
5. launch-plan env: the materialized system-prompt env and the account-home override
6. launch identity from `exec_identity_env`: `RIMZ_AGENT_KIND`, `RIMZ_AGENT_ID`, `RIMZ_RUN_ID`, `RIMZ_AGENT_NAME`, and the env-backed launch parameters (`RIMZ_AGENT_ROLE`, `RIMZ_TEAM`, `RIMZ_LAUNCH_GROUP`, `RIMZ_LAUNCH_ORDINAL`, `RIMZ_CHANNEL`, `RIMZ_AGENT_PROFILE`, `RIMZ_AGENT_MODEL`, `RIMZ_AGENT_EFFORT`, `RIMZ_AGENT_BUDGET`)
7. subagent lockdown env, for supervised children (`AgentDefinition::lockdown_subagent_env`)
8. `RIMZ_LAUNCH_REMINDERS` (`ENV_LAUNCH_REMINDERS`), the rendered launch reminders (empty when none) for adapters whose extension carries them (`SystemTextChannel::ExtensionEnv`)
9. sandbox pins, on sandbox launches only ([sandbox.md](../sandbox.md#environment-pins) owns the key list)

Layers 4 and above beat the project env, so a trusted config can tune an agent's launch but cannot override the adapter's launch contract, the account binding, or RimZ identity.

The wrapper is skipped, and the provider argv runs directly with no shell rc env (layer 2), when the user has no launchable shell, `/usr/bin/env` is missing, a key cannot be written as an `env(1)` assignment, or the shell is csh-family. [`invalid_env_key`](../../../crates/rimz/src/harness/launch.rs) separately refuses the launch at compile time when any key is empty, contains `=`, or starts with `-`.

### Secret redaction

A project puts credentials in `[[agents]]` env, so `CompiledAgentProcess` and `AgentProcessStage` implement `Debug` by hand. The env map prints each key against `<redacted>`, and every `KEY=VALUE` token the wrapper added to `argv` prints as `KEY=<redacted>` (`redact_env_tokens`). Debug-formatting a compiled process in a log line, a panic message, or an error context never prints a launch env value.

[`rimz agents explain`](../../reference/cli/agents.md#explain-a-launch) is narrower. It passes the same trust gate and redacts only the keys in `CompiledAgentProcess.secret_keys`, which are the trusted project `[[agents]]` env keys, across the env map, provider and wrapper argv tokens, and sandbox set-pins. Matching is by key, not by value substring, so other launch env stays visible, and so do paths derived from a secret value. The reference owns the observable report.

## Storage

Both records are per-machine, under `$XDG_CONFIG_HOME/rimz/projects/<workspace_id>/`, and written with atomic temp-plus-rename through [`disk::atomic::write_bytes_atomically`](../../../crates/rimz/src/disk/atomic.rs).

`trust.toml` is the grant record:

```toml
project_root = "/home/me/code/query-engine"
surface_hash = "sha256:..."
surface_json = '{"agents":[...],"profiles":[...],...}'
granted_at   = "2026-05-23T12:34:56Z"
```

`surface_json` is the canonical JSON the hash was computed over, kept so a stale grant can report what drifted. A record without `surface_json` fails to parse; there is no hash-only fallback. [`granted_roots`](../../../crates/rimz/src/trust.rs) scans these records to discover roots that have ever been granted, and callers still re-check each root's live state before running anything.

`birth-prompt.toml` records a declined [birth prompt](#granting-trust): `dismissed_hash` and `dismissed_at`. The prompt stays suppressed while the live hash equals `dismissed_hash`, and a grant deletes the file.

## The surface diff

A `stale` report carries a field-level diff of the granted surface against the live one. [`executable_surface_diff`](../../../crates/rimz/src/trust.rs) walks the two canonical JSON values and yields `added`, `removed`, and `changed` leaves with their paths, indexing arrays as `[n]`. `rimz trust status` renders it under the state line (`~ tasks[2].prompt`, with a word-level diff for changed strings), `rimz trust grant` renders it before pinning so a re-grant is informed, and `--json` carries the entries in `surface_diff`.

## Granting trust

`rimz trust [status|grant|revoke] [--json]` is the direct surface ([`cli/trust.rs`](../../../crates/rimz/src/cli/trust.rs)). `status` is the default. `grant` computes the diff against any prior record, pins the live hash and surface, and deletes `birth-prompt.toml`. `revoke` deletes `trust.toml`, returning the workspace to `untrusted`, or `no_config` when the config is absent.

Three other entry points grant on the user's behalf:

| Entry point | When it offers | Behaviour |
| --- | --- | --- |
| Birth prompt (`prompt_project_trust` in `cli/room/mod.rs`) | a `rimz start` that births a new room, on a TTY stdin, outside a remote reconnect, for a workspace with no grant record and no dismissal for the live hash | lists the surface summary and asks, defaulting to no; a decline writes `birth-prompt.toml`; errors log a warning and never block the room |
| Inline offer (`offer_inline_grant`) | a manual `rimz loop fire` on a TTY, or a project task edit on a TTY, whose gate is closed | prints the state and surface diff to stderr, then asks |
| Own-mutation re-pin (`regrant_own_mutation`) | `rimz loop add --project`, `remove`, or `rename` changes the project config | re-pins without asking when the pre-edit state was `trusted` or `no_config` |

The birth prompt only covers never-granted workspaces; a `stale` workspace re-grants through `rimz trust grant` or an inline offer. The re-pin rule is what keeps loop edits honest: the command re-pins only its own edit on a surface this machine had already approved, and an `untrusted` or `stale` pre-state falls through to the inline offer, or to a message naming `rimz trust grant` when stdin is not a TTY.

## Adding a command-running field

A new field that can run a process must be projected into `ExecutableSurface`, with a case in the `hash_covers_every_documented_surface_field` unit test (`trust/tests.rs`). The test hashes one config per field and fails when two cases collide, so a field dropped from the projection shows up as a collision. Nothing checks the docs automatically: add the field to the table above and to the operator list in [security.md](../../guide/security.md#project-trust) in the same change.

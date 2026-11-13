# Project trust

> How RimZ decides whether a repository's `.rimz/config.toml` may run commands on this machine: the executable-surface hash, launch-time enforcement, the grant entry points, the stale-grant diff, and the records on disk. The code is `crates/rimz/src/trust.rs`, and [fleet.md](./fleet.md) is the map for this area. For users, [security.md](../../guide/security.md) owns the threat model, [configuration.md § Project config](../../guide/configuration.md#project-config) the config surface a repo ships, and the [`rimz trust` reference](../../reference/cli/hooks-trust.md#project-trust) the command.

Command-running project settings are inert until a human trusts them on this machine. A clone can ship agents, profiles, teams, loop tasks, hooks, language servers, account selections, and env in `<project_root>/.rimz/config.toml`, and each of those can run a command or change what one runs as. A grant pins a SHA-256 over every command-running field and stores the granted surface beside it. Every later trust read re-hashes the live config. When the hash differs, the workspace is `stale` and the grant stops applying until someone grants again.

The pin covers the surface itself because agents in the room can write `.rimz/config.toml`. An agent's edit or a pulled change to any command-running field breaks the hash, so neither can grant itself command execution at the next launch.

## States

[`TrustState`](../../../crates/rimz/src/trust.rs) is derived on every read from the live hash and the on-disk grant record. It is never stored.

| State | Condition | Effect |
| --- | --- | --- |
| `no_config` | No `.rimz/config.toml`, or the project root's `.rimz` is the RimZ home itself. | Nothing to gate. |
| `untrusted` | Config present, no grant record on this machine. | Every command-running field is inert. |
| `trusted` | Grant record present, and its hash matches the live surface. | Trusted surfaces apply at launch. |
| `stale` | Grant record present, and its hash differs. | Same as `untrusted`. The report carries the [surface diff](#the-surface-diff). |

`trust::status` re-hashes on every call, so `stale` is the auto-revoke: drift shows on the next read of the workspace, with no background sweep. `rimz trust status`, `rimz doctor`, and every launch gate call it. `trust::blocked_fix` is the fix text every trust-gated refusal prints. Its `stale` variant says the surface changed since the last grant.

## The executable surface

`trust::ExecutableSurface` projects the fields that can cause a process to run, and only those enter the hash. Each entry's name is hashed with it, so renaming a profile, team, task, agent entry, or server drifts the grant.

| Config table | Hashed keys |
| --- | --- |
| `[[agents]]` | `name`, `launch_command`, `env` |
| `[profiles.<name>]` | `agent`, `skills`, `mode`, `model`, `effort`, `fast`, `auto-compact`, `system-prompt-file`, `append-system-prompt-files`, `args` |
| `[subagents.profiles.<name>]` | the same keys as `[profiles.<name>]`, in the child-launch namespace |
| `[agents.teams.<name>]` | `layout`, `consensus-file`, `consensus` (the piece names), `append-system-prompt-files`. Per role: `role`, `profile`, `signals`, `mode`, `model`, `effort`, `fast`, `auto-compact`, `system-prompt-file`, `append-system-prompt-files`, `args` |
| `[tasks.<name>]` | `agent`, `workflow`, `prompt`, `prompt-file`, `check` (a shell string), `judge` (one table or an array, each with `agent`, `prompt`, `prompt-file`, `recheck`, `timeout`), `verify`, `max-attempts`, `on`, `worktree`, `mode`, `effort`, `fast`, `system-prompt-file`, `timeout`, `at`, `every`, `cron`, `signal`, `match`. A task's short form compiles at fire time to an inline workflow graph ([loops.md](./loops.md#the-compiled-graph)) whose checks, judges, agent step, and prompt assets all come from these keys; the step settings it adds without them (`budget`, `notes`, `throttle`, `capacity-wait`, `account`) run no command |
| `[[hooks]]` | `event`, `command` |
| `[env]` | every key and value |
| `[lsp.servers.<name>]` | `command` argv, `init-options` as canonical JSON with every object's keys sorted, and `editor-check-on-save` when set |
| `[accounts]` | every `<kind> = "<name>"` selection, because it redirects every agent's credentials |

The hash input is canonical JSON, and the wire format is `sha256:<hex>`. Struct field order is fixed, `BTreeMap` keys sort, and an unset `Option` serializes as `null`, so the same config always hashes to the same bytes.

A field added to the projection after grants existed is left out of the JSON while it is empty or unset, so that those grants keep their hash. Two profile and role keys also keep an older JSON name for the same reason.

| Projection | Left out when |
| --- | --- |
| top level | `lsp_servers`, `subagent_profiles`, and `accounts` are empty |
| profile and subagent profile | `skills`, `auto_compact`, and `fast` are unset |
| team | `consensus_file` is unset, or `consensus` or `append_system_prompt_files` is empty |
| team role | `signals` is empty, or `auto_compact` and `fast` are unset |
| task | `workflow` and `fast` are unset, or `judge` is empty |
| language server | `editor_check_on_save` is unset |

A profile's and a role's `append-system-prompt-files` serialize under the singular key `append_system_prompt_file`. A team's list keeps the plural key. An explicit `fast = false` hashes differently from an unset `fast`, because it shapes provider argv and cost.

### What stays outside the hash

Every other key deserializes leniently and never touches the hash. `trust::ProjectConfig` ignores unknown keys, so display keys such as `display_name` and `sidebar_width` pass through. The project loaders accept these keys, and the hash leaves them out because they shape RimZ's own policy, display, or scheduling, not the command that runs:

| Table | Unhashed keys |
| --- | --- |
| `[agents]` | `env-reminder`, which a trusted overlay applies |
| team | `keep-warm`, `leader`, `scratch-files`, `stages` |
| team role | `owns`, `budget`, `keep-warm`, `flip-compact`, `idle-compact`, `smart-compact` |
| profile and subagent profile | `budget`, `keep-warm`, `idle-compact`, `smart-compact`, `empty-dir`, `description`, `subagents`, `model-reminder` |
| task | `label`, `notes`, `team`, `check-timeout`, `max-strikes`, `once-per-checkout`, `account`, `budget`, `budget-per-day`, `surplus`, `surplus-after`, `throttle`, `capacity-wait`, `overlap`, `overlap-limit` |
| language server | `kind`, `extensions`, `root-markers`, `policy`, `wait-timeout`, `memory-estimate` |
| `[packs]` | `expect` |

The overlap keys decide how many already-approved triggers run the approved command. They do not change the command, so editing them does not re-key a grant. Two rows depart from that rule. A task's `account` picks which machine account each fire runs on, while the `[accounts]` table, which does the same for the room, is hashed. A task's `check-timeout` bounds its shell check, while the task's `timeout` is hashed. The `Profile` rustdoc in `crates/rimz/src/config/agents.rs` states the rule for a new profile field: hash it, mark it `#[serde(skip)]`, or keep it off every command.

`crates/rimz/src/trust.rs::project_packs` reads `[packs]` through the same project reader in every trust state, preserving the exclusion of a `.rimz` that is the machine home. The table is retained as a lenient TOML value, so malformed expectations never fail the trust read. `crates/rimz/src/packs/mod.rs::expectation_warnings` checks the source grammar and installed names, and `crates/rimz/src/room/judgment.rs::judge` records its messages under `packs.expect` after definition warnings. An unreadable project config or installed-packs record adds no expectation warnings. Expectations execute nothing, and installing remains a separate reviewed command. The user-facing contract is [expected packs](../../guide/configuration.md#expected-packs).

Machine policy stays outside the hash because a repository cannot set it. Per-machine `[[notifications.handler]]` and `[notifications].command`, per-machine profiles, subagent profiles, and teams, per-machine loop `check` commands, `[agents.worktree.hooks]`, `agents.isolation`, `agents.runtime-env`, and `agents.shell` all live under `~/.rimz/` and are never trust-tracked. A project copy of any of these tables is ignored, and the trust tests pin that the notification, worktree-hook, and `runtime-env` copies leave the hash alone. A project `[throttle]` or `[keys]` table is also ignored, and room judgment (`crates/rimz/src/room/judgment.rs::judge`) warns about each one through `trust::project_throttle` and `trust::project_keys`.

A repository profile in either scope that sets `isolation` fails the trusted overlay with `RepoProfileSetsIsolation`. An untrusted workspace never reads repo profiles, so the refusal appears once the grant makes them live. The fix is a machine Markdown definition or `--isolation`. A repository therefore cannot choose host or scoped isolation. The [scoped mount view](../sandbox.md) is no substitute for trust: profile skill views do not make untrusted commands safe.

### Refused project keys

Some keys fail the read outright because they describe machine state or a retired shape. The When column says which reads check each one. The tier keys sit in the repo layer that only a grant makes live, so an untrusted workspace never reaches that check.

| Refused | Checked by | When | Message names |
| --- | --- | --- | --- |
| a `[layout]` table, including `[[layout.initial_panes]]` and `[layout.tmux]` | `trust::check_project_config_removed_tables` | every trust read | `placement` under `[agents]`, and teams in `<agents_home>/teams/<name>.md` |
| a retired key: `[agents]` `git-reminder` or `max-launch-depth`, a profile's or role's singular `append-system-prompt-file`, or a team-level `signals` | the same check, through `config::retired_agents_key` | every trust read | the key's replacement |
| `[tiers]`, `[models]` | `config::effective::load_with_roots` | any state with config | the move to `~/.rimz/config.toml` |
| `[lsp]` memory policy: `reserve-percent`, `reserve-min`, `idle-timeout`, `recycle-factor`, `recycle-min` | `config::effective::load_with_roots` | any state with config | the move to `~/.rimz/config.toml` ([language servers](../lsp.md#configuration)) |
| `tier` on a profile, subagent profile, or team role, or a tier name as its `model` | `reject_project_tier_key` and `reject_project_tier` in `crates/rimz/src/config/effective.rs` | `trusted` only | a concrete model |
| project task fields that make machine-local claims | `reject_project_task_state_fields` in `crates/rimz/src/config/effective.rs` | every project task load | [loops.md § Where tasks live](./loops.md#where-tasks-live) owns the list |

### How far the hash reaches

The hash covers repository definitions and what they reference by name, never the contents of a machine definition. A repo profile may inherit only repo profiles or built-in kinds (`RepoProfileEscapesTrust` otherwise), and a repo team role binds only repo profiles. A repo profile or team therefore runs only hashed definitions. A repo task always runs at the project root, but its `agent` may name a machine profile. The task's reference to it is hashed, and the machine profile's contents follow the machine config.

## Launch-time enforcement

Three gates apply trust at launch.

- The env gate is in the process compiler. `compile_agent_process_with_extra_env` in `crates/rimz/src/harness/launch.rs` resolves `[[agents]]` env through `trusted_agent_env`, and every compile path enters it. Interactive agents, supervised `-p` runs, and [resume-on-rebirth seeds](../sidebar/sidebar.md#resume-on-rebirth) compile there through `launch_plan::prepare_exec` in the hidden `rimz agents exec` wrapper ([`exec.rs`](../../../crates/rimz/src/cli/agents_cmd/exec.rs)). Headless judges reach `prepare_exec` through `harness::headless::prepare` and run without the wrapper. `preflight_agent_process` (restart, fork, molt, subagent resume, and the launcher's cell check) and `compile_managed_agent_process` (supervised preparation and the loop runner's managed spawn) compile the same way.
- The profile and team gate is `LaunchAgents::block_untrusted_reference` in `config::effective`, which `harness::plan` and `harness::team_stage` call when they resolve a spec.
- Loop tasks pass the task gate before firing.

| Surface | `trusted` | `untrusted` or `stale` |
| --- | --- | --- |
| `[[agents]]` `env` | injected into the agent process | launch refuses with the grant fix |
| `[profiles]` | overlaid on machine profiles, winning name collisions | a spec that uses a repo profile refuses |
| `[subagents.profiles]` | overlaid on machine subagent profiles, winning name collisions | a child spec that uses a repo subagent profile refuses |
| `[agents.teams]` | overlaid on machine teams | a spec that uses a repo team refuses |
| `[accounts]` | live account defaults for unpinned providers | resolution refuses each inheriting kind with the grant fix. Pinned kinds ignore the project. |
| `[tasks]` | overlaid on machine loop tasks and state instances, then gated by machine-local enablement | the project task is filtered out, and a same-named machine task keeps running |
| `[lsp.servers]` | overlaid on machine servers, whole entries by name | warns and stays inert, and same-named machine entries stay in effect |
| `[[agents]]` `launch_command`, `[[hooks]]`, top-level `[env]` | hashed, not consumed | hashed, not consumed |

The last row is hashed ahead of use, so consuming those fields at launch will need no new grant.

`trust::agent_env` resolves the `[[agents]]` env for one kind. Entries sharing a name merge in declaration order, later entries win key collisions, and values pass literally with no shell expansion. It returns `AgentEnv::Blocked` only when an entry for that kind carries env and the workspace is untrusted or stale. A kind with no env entry launches in an untrusted checkout. `harness::launch::trusted_agent_env` turns `Blocked` into a `BlockedEnv` compile error, and the preflight runs the same compile, so the launch fails before any pane opens. Project config uses one `agents` shape at a time: the `[[agents]]` array for env entries, or the `[agents]` table for `[agents.teams]`.

The profile, team, and task overlays live in [`config::effective`](../../../crates/rimz/src/config/effective.rs). On a closed gate, `LaunchAgents::block_untrusted_reference` refuses only a spec that would consume a repo profile or team. Machine profiles and built-in kinds keep launching in an untrusted checkout that merely declares project config.

A trusted project task still needs its own arming. Trust approves the config contents, and `rimz loop enable <name>` approves that task for unattended execution on this machine. [loops.md § Where tasks live](./loops.md#where-tasks-live) owns that second approval.

### Env application

`harness::launch::compose_agent_env` layers the launch env, and the login-shell wrapper (`login_shell_argv_with`) delivers it. The wrapper runs the launch shell's startup files, then execs `/usr/bin/env` with RimZ's launch env as `KEY=VALUE` arguments, so the launch env wins over anything the rc files set. Precedence, lowest to highest:

1. pane env
2. shell rc and profile env
3. trusted project `[[agents]]` env
4. adapter launch built-ins (`LaunchCapability::launch_env`)
5. launch-plan env from `launch_plan::compile`: the materialized system-prompt env, the account-home override, `RIMZ_WORKTREE_PATH` when the launch records a checkout, and the per-launch keys [sandbox.md § Environment pins](../sandbox.md#environment-pins) tables (`RIMZ_ISOLATION`, `TMPDIR`, `RIMZ_USER_TMPDIR`, the adapter's temp-root keys, `RIMZ_TEMP_ROOT_KEYS`, `RIMZ_SHARED`, `ZELLIJ_SOCKET_DIR`). When the machine sets `[agents] shell`, its keys go on top: `SHELL` and `CLAUDE_CODE_SHELL` name the resolved path (a full configured path, the room's matching saved lookup, or the launch's own PATH hit), and `RIMZ_USER_SHELL` keeps the user's own shell for shell panes a nested `rimz` opens.
6. launch identity from `exec_identity_env`: `RIMZ_AGENT_KIND`, `RIMZ_AGENT_ID`, `RIMZ_RUN_ID`, `RIMZ_AGENT_NAME`, and the env-backed launch parameters (`RIMZ_AGENT_ROLE`, `RIMZ_TEAM`, `RIMZ_LAUNCH_GROUP`, `RIMZ_LAUNCH_ORDINAL`, `RIMZ_AGENT_PROFILE`, `RIMZ_AGENT_MODEL`, `RIMZ_AGENT_EFFORT`, `RIMZ_AGENT_BUDGET`)
7. provider config env overrides: `LaunchCapability::finalize_launch_env` runs once for every managed launch. It reads the effective login env after the composed layers above, receives the native-delegation decision and optional composed system-prompt artifact, and writes all config edits together into the final env. OpenCode's [adapter mapping](../agents/adapter_opencode.md#launch-and-resume) owns the `OPENCODE_CONFIG_CONTENT` prompt replacement and delegation restriction, the `OPENCODE_CLI_CONFIG_CONTENT` tab override, and their launch warnings.
8. the compiler's own switches: `RIMZ_LAUNCH_REMINDERS`, the rendered launch reminders, for adapters whose extension carries them (`SystemTextChannel::ExtensionEnv`). `RIMZ_LAUNCH_FAST` is `1` when effective fast is on and the adapter uses `FastSwitch::ExtensionEnv` (Pi). Every other launch removes it and lists it in `unset`, including an inherited value. `RIMZ_LAUNCH_AUTO_APPROVE` is `1` when a frontend replaces its native auto-approve flag with once-only extension replies (OpenCode mini). Every other launch removes it and lists it in `unset`, including an inherited value. `RIMZ_RUNTIME_ENV` is `1` on a launch that gets its [Environment at prompt submit](./fleet.md#environment-at-prompt-submit) and unset on every other. A qualifying Pi keep-warm launch sets `PI_CACHE_RETENTION=long` and `RIMZ_LONG_CACHE_ENV=PI_CACHE_RETENTION=long` unless the retention is user-owned. A key set in trusted project `[[agents]]` env is always user-owned, even when an inherited marker matches its value. The marker records `KEY=VALUE`, split at the first `=`. Any other inherited value is RimZ-owned only while that key still equals that value. Every launch strips an inherited RimZ-owned pair it does not request, preserves user values, and unsets the marker unless it sets it anew ([long prompt cache](../agents/adapter_pi.md#long-prompt-cache)). `RIMZ_TEAM` and `RIMZ_AGENT_ROLE` are unset when the identity has no team or no role, so a launch never carries its caller's.
9. launch-plan pins, at every level (`CompiledAgentProcess::pin_env` from `launch_plan::compile`): an inherited temp-root key the adapter does not own is unset unless the compiled env sets it, and `RIMZ_WORKFLOW_RUN`, `RIMZ_WORKFLOW_DIR`, and `RIMZ_WORKFLOW_DEFINITION` are unset, then set again for a workflow attempt (`RIMZ_WORKFLOW_DEFINITION` only when the attempt has a definition path). A headless launch also unsets every `RIMZ_AGENT_*` key, `RIMZ_CHANNEL`, and `RIMZ_RUNTIME_ENV`, then pins the workspace identity, `RIMZ_WORKTREE_PATH`, and its loop task.
10. sandbox pins, on scoped launches only ([sandbox.md](../sandbox.md#environment-pins) owns the key list)

Layers 4 and above beat the project env. A trusted config can tune an agent's launch, but it cannot override the adapter's launch contract, the account binding, or RimZ identity.

Bash runs as `-i -c`, and POSIX shells and fish run as `-l -i -c`. The wrapper is skipped, and the provider argv runs directly with no shell rc env (layer 2), in four cases: there is no launch shell, `/usr/bin/env` is missing, a set or unset key cannot be written as an `env(1)` argument, or the shell is csh-family. The launch shell is the resolved machine `[agents] shell`, else `proc::user_shell()`. `resolve_agent_shell` applies a matching room lookup while its path is still a file, otherwise falling back to the launch's PATH, and validates the chosen path. It refuses relative paths containing a slash, missing programs, `false` or `nologin`, and csh-family shells. A valid configured shell still skips the wrapper when `/usr/bin/env` is missing. `compose_agent_env` refuses the launch at compile time with `InvalidEnvKey` when a key in layers 3 to 6 is empty, contains `=`, or starts with `-` (`invalid_env_key`). A key added after that check takes the skip path instead. [fleet.md § The exec wrapper](./fleet.md#the-exec-wrapper) owns the wrapper's lifecycle.

### Secret redaction

A project puts credentials in `[[agents]]` env, so `CompiledAgentProcess` and `AgentProcessStage` implement `Debug` by hand. The env map prints each key against `<redacted>`, and each wrapper `argv` token `KEY=VALUE` whose key is in the env prints as `KEY=<redacted>` (`redact_env_tokens`). `provider_argv` prints unchanged, so a secret value that also appears as a bare provider argument or path is not hidden. Within that limit, a compiled process in a log line, a panic message, or an error context prints no launch env value. Redaction covers RimZ's own output only. The wrapper passes every launch env value as a `KEY=VALUE` argument to `/usr/bin/env`, so a same-user process listing shows the values while the shell starts. The `login_shell_argv_with` rustdoc records this, and names a two-stage re-exec as the fix a future secret source needs.

[`rimz agents explain`](../../reference/cli/agents.md#explain-a-launch) passes the same trust gate and redacts by key, only the keys in `CompiledAgentProcess.secret_keys`. Those are the trusted project `[[agents]]` env keys. It redacts them across the env map, the provider and wrapper `KEY=VALUE` tokens, and sandbox set-pins. One exception: an env-map value equal to the resolved account home stays readable, since it is RimZ's own override and not the project secret it shadows. Other launch env stays visible, and so do paths derived from a secret value. The reference owns the observable report.

## Granting trust

`rimz trust [status|grant|revoke] [--json]` is the direct surface ([`cli/trust.rs`](../../../crates/rimz/src/cli/trust.rs)), and `status` is the default. `grant` pins the live hash and surface, deletes `birth-prompt.toml`, and clears the folder-trust decline. It then prints the report, with the diff against any prior record, so a grant made over a stale record shows what it just approved. It asks nothing before the project grant. On a TTY it then offers folder trust for undecided kinds ([below](#provider-folder-trust)). On a project without config, `grant` records nothing and deletes no birth-prompt record, but still clears the folder-trust decline. `revoke` deletes `trust.toml`, which returns the workspace to `untrusted`, or `no_config` when the config is absent.

Three other entry points grant on the user's behalf:

| Entry point | When it offers | Behaviour |
| --- | --- | --- |
| Birth prompt (`prompt_project_trust` in `cli/room/mod.rs`) | a `rimz start` that births a new room, attended (stdin is a TTY and the start is not a remote reconnect), for a workspace with no grant record and no dismissal for the live hash | lists the surface summary and asks, defaulting to no. A decline writes `birth-prompt.toml`. Errors log a warning and never block the room. |
| Inline offer (`offer_inline_grant` in `cli/trust.rs`) | a manual `rimz loop fire` on a TTY whose project task is gated, or a project task edit on a TTY that the re-pin below declined | prints the state and surface diff to stderr, then asks. A `no_config` state asks nothing. |
| Own-mutation re-pin (`regrant_own_mutation` in `cli/trust.rs`) | `rimz loop add --project`, or `rimz loop remove` or `rename` of a project task, changes the project config | grants the whole live surface without asking, when the state before the edit was `trusted` or `no_config` |

The birth prompt checks for a grant record at the time of the start, so a revoked workspace is offered again unless a dismissal matches the live hash. A `stale` workspace still has its record, and grants through `rimz trust grant` or an inline offer.

The re-pin rule rests on the state before the edit, not on a comparison of surfaces. The command trusts that a surface this machine had approved, plus its own edit, is what the user wants pinned. It grants the whole live config, so an unrelated edit made between the pre-edit read and the grant is pinned too. An `untrusted` or `stale` state before the edit falls through to the inline offer on a TTY, or to a message naming `rimz trust grant` otherwise.

## The surface diff

A `stale` report carries a field-level diff of the granted surface against the live one. `trust::executable_surface_diff` walks the two canonical JSON values and yields `added`, `removed`, and `changed` leaves with their paths, indexing arrays as `[n]`. `rimz trust status` and the inline offer render it under the state line (`~ tasks[2].prompt`, with a word-level diff for changed strings). `rimz trust grant` renders the diff it computed against the prior record. `--json` carries the entries in `surface_diff`.

## Storage

Trust records are per-machine, under `~/.rimz/trust/<workspace_id>/`, and each is written atomically through [`disk::atomic::write_bytes_atomically`](../../../crates/rimz/src/disk/atomic.rs).

| File | Fields | Written by | Removed by |
| --- | --- | --- | --- |
| `trust.toml` | `project_root`, `surface_hash`, `surface_json`, `granted_at` | every grant on a project with config | `rimz trust revoke` |
| `birth-prompt.toml` | `dismissed_hash`, `dismissed_at` | a declined birth prompt | a grant on a project with config |
| `folder-trust-prompt.toml` | `dismissed_kinds`, `dismissed_at` | an attended `rimz start` that declines the folder-trust offer, or that shows only undecided kinds it cannot grant and so records them without asking | `rimz trust grant` without `--empty-dir`, and an accepted folder-trust birth offer |

```toml
project_root = "/home/me/code/query-engine"
surface_hash = "sha256:..."
surface_json = '{"agents":[...],"profiles":[...],...}'
granted_at   = "2026-05-23T12:34:56Z"
```

`surface_json` is the canonical JSON the hash was computed over, kept so a stale grant can report what drifted. A record without `surface_json` fails to parse, and there is no hash-only fallback. `trust::granted_roots` scans these records to discover roots that have ever been granted. Its callers still re-check each root's live state before running anything.

The birth prompt stays suppressed while the live hash equals `dismissed_hash`, so any change to the surface offers it again.

The folder-trust decline merges kind names across declines, so a newly undecided kind gets an offer without reopening old declines. Library grants, including the re-pins `rimz loop` makes after its own edits, leave it standing. Missing or malformed dismissal data reads as no dismissal: it can cause another offer, never a grant. `trust::dismiss_folder_trust_prompt`, `trust::dismissed_folder_trust_kinds`, and `trust::clear_folder_trust_dismissal` own the record.

## Provider folder trust

Folder trust is a provider's own per-directory decision, such as Claude's accepted trust dialog or Codex's `trust_level`. It is separate from project trust. `rimz trust` reports both, and RimZ writes a folder-trust grant only through the agents capability layer (`InstallationCapability::folder_trust`, then each adapter's `folder_trust.rs`).

| Writer | What it grants |
| --- | --- |
| `rimz trust grant --agents [<kind>,...]` | the named kinds, or every undecided kind, at this project's grant key |
| `rimz trust grant` on a TTY | the undecided kinds the user confirms after the preview |
| `rimz trust grant --empty-dir` | the machine-wide empty-directory root, and not project trust |
| fresh eligible `empty-dir` profile, explicit child `--empty-dir`, or undecided-cwd fallback | an undecided, grantable account whose key is exactly the resolved empty root. The profile key or child launch is consent, with no question or diff prompt |
| attended `rimz start` | the undecided kinds the user accepts, offered after account resolution and before room birth judges remote-control readiness |

Explicit grants and attended birth offers preview the provider-file diff through the shared CLI file-diff renderer. A fresh eligible `empty-dir` launch instead prints the trusted folder, account, and changed provider file, and appends an `empty_dir_grant` assist. Every write checks that its preview is still fresh before an atomic replacement. Automatic empty-root grants re-read a changed file and retry at most three writes. Profile-only write errors or continued changes fall back with a notice, and an explicit child empty start or undecided-cwd fallback instead refuses. That check is not a compare-and-swap against a provider writing the file at the same time. Saved-untrusted decisions, non-grantable files, and grant keys wider than the empty root stay manual. Re-entry never grants automatically.

Codex's saved-untrusted decision is `Restricted`, and no grant path writes over it. `rimz start` prints its fix on every start without asking and records no decline for it. `rimz doctor` and `rimz trust` resolve each kind's account as a launch would: room pins and live inherited defaults inside a room, else the trusted project, then the machine, then the provider default. A kind whose account fails to resolve does not hide its siblings' rows.

The verdict each launch class gets for an undecided or restricted folder (warn, refuse, or start privately for an explicit-cwd child) is owned by [fleet.md § Every path that builds a launch layout](./fleet.md#every-path-that-builds-a-launch-layout). Each provider's decision and grant key are on its adapter page: [Claude](../agents/adapter_claude.md#folder-trust), [Codex](../agents/adapter_codex.md#directory-trust-preflight), and [Qoder](../agents/adapter_qoder.md#launch-preconditions-and-folder-trust). The launcher owns both recipes: workflow leaders take the strict run-bearing recipe, and companions take the loop's verdicts (undecided warns, and Restricted and untrusted hooks refuse), naming the workflow in refusals.

## Adding a command-running field

A new field that can run a process must be projected into `ExecutableSurface`, with cases in the `hash_covers_every_documented_surface_field` unit test (`crates/rimz/src/trust/tests.rs`). The test hashes one config per case and fails when two cases collide, so a field dropped from the projection shows up as a collision.

A case proves its field only when another case is identical but for that field. Add a field as a pair: the bare case for its surface, and one that differs from it only in the new field. Add the pair once per surface that carries the field. The cases are hand-listed, so a projected field that nobody wrote a case for is the one gap this test cannot catch. A field left out of the projection on purpose gets an assertion that it does not move the hash, as the test does for profile `empty-dir`.

If the field must not re-key existing grants, skip it when unset and add it to the [omission table](#the-executable-surface). Two tests pin literal hashes of existing configs, `lsp_default_trust_hash_is_unchanged` and `task_labels_do_not_change_the_executable_hash`, and fail when a projection change moves either config's hash. They cover one language-server config and one task config, not every surface. Nothing checks the docs automatically: add the field to the hashed-keys table above and to the operator list in [security.md](../../guide/security.md#project-trust) in the same change.

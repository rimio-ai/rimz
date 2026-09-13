# Agent plugin internals

A process plugin is a third-party agent that joins RimZ through a machine-tier bundle instead of compiled Rust. [`agents/adapters/plugin/`](../../../crates/rimz/src/agents/adapters/plugin/mod.rs) turns each bundle into an `AgentDefinition` with three inputs: a static manifest that becomes the `AgentSpec`, canonical JSON envelopes the agent's own shim pushes through `rimz hooks feed`, and optional short-lived probe executables RimZ pulls for spend, account, and version.

This page covers how RimZ handles those inputs. The bundle layout, every manifest field, the envelope fields, and the probe request and response shapes are the public contract in [agent-plugins.md](../../reference/agent-plugins.md); read that first. The registry and the capability traits a plugin plugs into are in [adapter.md](./adapter.md).

## Module map

| File | Owns |
| --- | --- |
| [`mod.rs`](../../../crates/rimz/src/agents/adapters/plugin/mod.rs) | `PluginAdapter`, its capability impls, and spec derivation (`build_descriptor`, `derive_coverage`, `derive_user_coverage`, `derive_lifecycle_hooks`) |
| [`manifest.rs`](../../../crates/rimz/src/agents/adapters/plugin/manifest.rs) | `PluginManifest` schema, `validate`, `valid_kind`, bundle-relative path resolution |
| [`load.rs`](../../../crates/rimz/src/agents/adapters/plugin/load.rs) | directory scan, `LoadedPlugins`, the process cache `loaded()`, doctor diagnostics |
| [`protocol.rs`](../../../crates/rimz/src/agents/adapters/plugin/protocol.rs) | protocol-1 `Envelope`, `CANONICAL_EVENTS`, `CanonicalEvent::normalize` |
| [`probes.rs`](../../../crates/rimz/src/agents/adapters/plugin/probes.rs) | bounded subprocess runner and the spend, account, and version mappings |
| [`check.rs`](../../../crates/rimz/src/agents/adapters/plugin/check.rs) | `check_from_root`: the authoring report behind `rimz agents check` |

Code outside `agents` reaches this module only through the provider-neutral façade [`agents/plugins.rs`](../../../crates/rimz/src/agents/plugins.rs).

## Loading and the registry

`load_from_root` builds every plugin in one pass over `$XDG_CONFIG_HOME/rimz/agents.d/*/agent.toml`:

1. Collect each subdirectory's `agent.toml` and sort the paths, so display order is deterministic.
2. Parse the TOML and run `PluginManifest::validate`: protocol version, kind grammar, no collision with a `registry::BUILTINS` kind, kind equal to the directory name, non-empty identity, unique and canonical `emits` containing `session_start`, editing tools a subset of mutating tools, a `{session_id}` placeholder in `resume`, non-empty probe argv, and an existing `setup-doc` file.
3. Reject a kind already taken by an earlier plugin in sort order.
4. Build and leak the adapter and its spec (`build_adapter`), then run the shared `AgentDefinition::validate`.

The result is `LoadedPlugins`: valid `definitions`, one `PluginLoadError` per rejected manifest, and one `PluginDiagnostic` per manifest either way. An error's `kind_hint` is the directory name, which is how the hook feed matches a broken bundle to its `--source`.

`loaded()` runs the load once per process behind a `OnceLock` and logs one warning per rejected manifest. `registry::all_definitions()` chains `BUILTINS` with `loaded().definitions`, so a valid plugin reaches every consumer that resolves agents through the registry (sidebar projection, spending discovery, resume, coverage, doctor, launches) with no plugin-specific branch. Authoring commands call `load_from_root` directly and get a fresh load.

A plugin's spec must outlive the process like a built-in's `const` spec, because `AgentDefinition` and `AgentSpec` hold `&'static` references. `build_adapter` therefore leaks the manifest, the bundle path, the spec, and every derived string and slice once. The leak is bounded by machine configuration; it also covers a manifest that fails `AgentDefinition::validate` after building. Live manifest reload would require moving the registry to owned shared values.

## The derived spec

`build_descriptor` fills the `AgentSpec` from the manifest, and a plugin cannot set what the manifest has no field for. The values RimZ fixes for every plugin:

| Spec field | Plugin value |
| --- | --- |
| `bin_names` | the kind alone |
| `aliases`, `sub_providers`, `expected_windows` | empty |
| `plan_label` | `TitleCaseOnly` |
| `capabilities.native_ask_ui`, `registers_lazily` | from `[capabilities]` |
| `capabilities.direct_account_usage` | true when an account probe is declared |
| `capabilities.transcript_tail_context`, `local_session_discovery`, `daemon_hooked_sessions`, `remote_control` | off |
| `capabilities.same_pane_session` | `KeepPrimary` |
| `launch.prompt` | positional after `--` |
| `launch.resume`, `fork`, `max_turn_flag`, `presets.auto_compact` | none; resume argv comes from `resume_command` instead |
| `launch.compact_command` | the manifest string, with `CompactInstruction::Unsupported` |
| `default_model`, `default_context_window` | none |
| `thread_key` | `[transcripts].thread-key`, default `PerFile` |

The installation, runtime-control, session, and transcript capability traits are empty impls, and `skills_home` returns `None`. `launch_command` and `resume_command` resolve `argv[0]` against the bundle directory when it contains a path separator, using the same rule as probes.

### Declared coverage

The coverage matrices are derived from `emits`, the capability flags, and probe presence, so the published claims follow what the bundle declares. Each concern is `Wired` when its condition holds and `Unsupported` otherwise:

| Concern | Wired when |
| --- | --- |
| `turn_lifecycle` | `emits` has `session_start`, `turn_start`, and `turn_end` |
| `permission`, `plan_approval`, `user_question` | `emits` has `awaiting_input` and `native-ask-ui = true` |
| `compaction` | `emits` has `compaction_start` and `compaction_end` |
| `subagents` | `subagents = true` and `emits` has `subagent_start` and `subagent_end` |
| `session_end` | `emits` has `session_end` |
| `context_usage` | `context-usage = true` or `emits` has `context` |
| `realtime_cost`, `rich_context` | `emits` has `context` |
| `account_spend` | a spend probe is declared |
| `idle_notification` | always `Partial`, via `turn_end` and the stall window |
| `answer`, `launch_reminders`, `background_parking`, `hook_install`, `tool_stats`, `remote_control` | never; `hook_install` names the bundle's setup doc as the reason |

`context_usage` is the one row a capability flag can wire without a matching event. Every lifecycle hook row is `Native` exactly when its canonical event is in `emits`, except `lost`, which is always `Derived` through the `rimz exec` wrapper. The user-capability marks follow the same conditions; `live`, `history`, `account`, and `ask` stay `Partial` at best, because what lands depends on what the plugin sends. [adapter.md#declared-coverage](./adapter.md#declared-coverage) defines both matrices.

`emits` constrains claims, never ingest: a valid canonical event missing from `emits` still decodes.

## Hook ingest

The shim runs `rimz hooks feed --source <kind> --event <event>` with one envelope on stdin. `run_feed` in [`cli/hooks.rs`](../../../crates/rimz/src/cli/hooks.rs) resolves the plugin's definition like any adapter's and calls `PluginAdapter::decode_hook`, which returns one `HookOutput` and never a JSON reply, so hook stdout stays empty.

`decode_hook` runs these steps:

1. `Envelope::parse` deserializes the payload once. It fails on a shape serde rejects (including a known event with malformed typed fields), a `protocol` other than 1, or a `hook_event_name` that differs from the feed's event name. A failure logs at debug level and returns class `Unknown` with no observation.
2. An event name absent from `emits` logs `warn_undeclared_once`, once per kind and event. A name outside `CANONICAL_EVENTS` parses as `CanonicalEvent::Unknown`, can never be in `emits`, and so takes this warning too before returning class `Unknown` with no signal.
3. A `tool_use` counts as mutating when `is_error` is false and `tool_name` is in the spec's mutating tools, and as editing when it also appears in the editing tools.
4. `CanonicalEvent::normalize` maps the event to its class, ask kind, `LifecycleSignal`, ask questions, turn error, final message, and the progress and session-ended flags.
5. Routing uses `agent_id`, falling back to `session_id`, as the route key and `session_id` as the session, with `cwd` as the worktree.
6. A `context` event attaches a `ContextObservation`. `normalize_context` stamps `source` and `observed_at` and folds top-level `model`, `total_cost_usd`, `rate_limits`, and token fields into the `AgentContext` shape. The observation needs a root identity: a context envelope whose `agent_id` differs from its `session_id` is dropped.
7. Identity comes from the shared helpers in [`agents/identity.rs`](../../../crates/rimz/src/agents/identity.rs). `subagent_start` and `subagent_end` need a distinct child `agent_id` and parent `session_id`; anything else is quarantined with an error on the `rimz::agent::lifecycle` target. A root event carrying an `agent_id` different from its `session_id` is dropped as a foreign child.
8. The `AgentLifecycleObservation` carries model, effort, usage (`context_pct` capped at 100), transcript path, and the `turn_start` prompt as both task and prompt.

`awaiting_input` is where `native-ask-ui` changes behaviour. The event always decodes as `AwaitingUser`, but `run_feed` writes an ask to the store only when the spec declares `native_ask_ui`. With the flag off the feed records nothing (no waiting state, no transcript ask, no usage from that envelope) and returns the same empty reply, because no native prompt exists for a waiting row to route you to. Built-ins all declare the flag, so this branch runs only for a plugin; [adapter.md#two-channels](./adapter.md#two-channels) describes the ask channel it gates. The coverage rows for asks go `Unsupported` in the same case.

`rimz hooks feed` is the only delivery path protocol 1 has. The envelope carries nothing specific to process execution, so the vocabulary does not depend on it.

## Probes

Probes are pull-only enrichment, run by `run_json_diagnostic` in `probes.rs` with these bounds:

| Aspect | Behaviour |
| --- | --- |
| Executable | `resolve_executable`: an absolute path as given, a path with a separator joined to the bundle directory, a bare name looked up on `PATH` |
| Working directory | the bundle directory |
| stdin | piped; the spend request as one JSON line, empty for account and version |
| stdout | piped, capped at 1 MiB |
| stderr | piped, capped at 16 KiB, quoted in the error on nonzero exit |
| Deadline | 3 seconds, then the child is killed |
| Failure | spawn error, request write failure, timeout, nonzero exit, oversized output, or invalid JSON logs a warning and yields the empty result below |

| Probe | Adapter method | Maps to | Empty result |
| --- | --- | --- | --- |
| spend | `SpendingCapability::parse_spend` | `CachedEntry` per entry with an RFC 3339 timestamp; `cost_usd` taken verbatim (non-finite becomes 0) and the `PriceBook` ignored; `SpendCursor.state` round-trips the plugin's opaque cursor and `SpendCursor.offset` is the file length at parse time | `SpendParse::default()` |
| account | `AccountCapability::probe_account`, `probe_account_usage` | `AgentAccount` with plan and account id; usage adds `rate_limit_windows` and keys the account by `account_id`; `logged_out` or an all-empty response means logged out | `Unavailable` and `Failed`; with no account probe declared, `LoggedOut` and `Unsupported` |
| version | `LaunchCapability::probe_version` | first non-empty stdout line | `None` |

Spend discovery needs both halves: `spending_sources` returns sources only when the manifest has `[transcripts]` and a spend probe, while `transcript_files` lists glob matches from `[transcripts]` alone. A glob starting `~/` expands from `HOME`, an absolute glob stands as written, and a relative glob resolves from the bundle directory.

Declaring an account probe sets `direct_account_usage` without running the probe; the probe runs only when a consumer asks for the account.

## Invalid manifests at each entry point

A broken bundle is a failed precondition for a room and an absent agent everywhere else:

| Entry point | Behaviour |
| --- | --- |
| `rimz start` and detached start (`validate_agent_plugins` in [`cli/room/mod.rs`](../../../crates/rimz/src/cli/room/mod.rs)) | refuse after sandbox preflight and before resolving the workspace, listing every error and pointing at `rimz agents register --check` |
| Registry reads through `all_definitions()` | see valid plugins only; `loaded()` warned once per rejected manifest |
| `rimz hooks feed --source <kind>` | when the kind is not a definition and a load error's `kind_hint` matches, warn and exit 0 with empty stdout; the body is read and parsed first, so non-JSON input still fails the command |
| `rimz doctor` | one row per `PluginDiagnostic`: validity, error, setup doc, and each probe's presence and executable bit |

## Authoring commands

`rimz agents register <kind>` ([`cli/agents_cmd/register.rs`](../../../crates/rimz/src/cli/agents_cmd/register.rs)) checks the kind grammar and rejects a built-in kind or an existing directory. It then creates the bundle directory in place and writes `agent.toml`, `README.md`, `shim.sh`, `probes/spend`, and `probes/account`, each file atomically, removing the directory if any write fails. `register --check` runs a fresh `load_from_root` over the whole machine registry and exits nonzero listing every error.

`rimz agents check <kind>` calls `check_from_root`. It refuses a built-in kind, loads fresh, and fails with the manifest error if that kind was rejected. The report counts both coverage matrices, checks every declared probe for presence and the executable bit, runs the account and version probes for real, and runs the spend probe only with `--spend-file`. `--replay <jsonl>` parses each line with `Envelope::parse_diagnostic`, which returns the reason `parse` hides, then decodes it through `decode_hook` and steps a per-agent in-memory state with `agents::step`, without opening a store. A line that fails parsing, yields no observation, or lacks an agent identity counts as rejected, and any rejection or failed probe fails the command.

## See also

- [agent-plugins.md](../../reference/agent-plugins.md): the bundle, manifest, envelope, and probe contracts.
- [adapter.md](./adapter.md): the registry, the spec, the hook path, and declared coverage.
- [instances.md](./instances.md): how a registered session binds to its pane.
- [providers.md](./providers.md): the account and spend consumers the probes feed.

# Agent plugin internals

A process plugin is a third-party agent that joins RimZ through a bundle directory instead of compiled Rust. [`agents/adapters/plugin/`](../../../crates/rimz/src/agents/adapters/plugin/mod.rs) turns each bundle into an `AgentDefinition` from three inputs: a static manifest that becomes the `AgentSpec`, canonical JSON envelopes that the agent's own shim pushes through `rimz hooks feed`, and optional short-lived probe executables that RimZ runs for spend, account, and version.

The public contract is [agent-plugins.md](../../reference/agent-plugins.md): the bundle layout, every manifest field, the envelope fields, the probe request and response shapes and their limits, and the coverage claims. Read it first. This page covers how the code handles those inputs and what a change must keep true. The registry and the capability traits the adapter implements are in [adapter.md](./adapter.md).

A plugin registers a kind. [Markdown definitions](../../reference/definitions.md) are a separate surface: they configure profiles and teams that use a kind, and they never register one.

## Module map

| File | Owns |
| --- | --- |
| [`mod.rs`](../../../crates/rimz/src/agents/adapters/plugin/mod.rs) | `PluginAdapter`, its capability impls, and spec derivation (`build_descriptor`, `derive_coverage`, `derive_user_coverage`, `derive_lifecycle_hooks`) |
| [`manifest.rs`](../../../crates/rimz/src/agents/adapters/plugin/manifest.rs) | `PluginManifest` schema, `validate`, `valid_kind`, bundle-relative path resolution |
| [`load.rs`](../../../crates/rimz/src/agents/adapters/plugin/load.rs) | `plugins_root`, the directory scan, `LoadedPlugins`, the process cache `loaded()`, doctor diagnostics |
| [`protocol.rs`](../../../crates/rimz/src/agents/adapters/plugin/protocol.rs) | protocol-1 `Envelope`, `CANONICAL_EVENTS`, `CanonicalEvent::normalize` |
| [`probes.rs`](../../../crates/rimz/src/agents/adapters/plugin/probes.rs) | the bounded subprocess runner and the spend, account, and version mappings |
| [`check.rs`](../../../crates/rimz/src/agents/adapters/plugin/check.rs) | `check_from_root`: the authoring report behind `rimz agents check` |

Code outside `agents` reaches this module only through the provider-neutral façade [`agents/plugins.rs`](../../../crates/rimz/src/agents/plugins.rs). Inside `agents`, `registry::all_definitions` is its one reader.

`probes.rs` counts as a spend parser for `cargo xtask invariants`. Like every adapter's `spend.rs`, it stays read-only: it may not import store-write, run-wake, or broker modules (`ensure_spend_parser_boundaries` in [`xtask/src/invariants.rs`](../../../xtask/src/invariants.rs)).

## Loading and the registry

`load_from_root` builds every plugin in one pass over `agents.d/*/agent.toml` under the RimZ home (`plugins_root`):

1. Collect. List each subdirectory's `agent.toml` and sort the paths, so display order is deterministic. A missing `agents.d` yields no plugins.
2. Validate. Parse the TOML and run `PluginManifest::validate`. The rules include protocol 1, the kind grammar (`valid_kind`), no collision with a `registry::BUILTINS` kind, a kind equal to the directory name, unique canonical `emits` that include `session_start`, editing tools that are a subset of mutating tools, a `{session_id}` placeholder in `resume`, a non-empty argv for each declared probe, and an existing `setup-doc` file. The reference's [manifest tables](../../reference/agent-plugins.md#manifest) state each rule as an author meets it.
3. Deduplicate. Reject a kind that an earlier path in sort order already took. Step 2 ties each kind to its directory name, so this check is a backstop that cannot fire today.
4. Build. `build_adapter` builds and leaks the adapter and its spec, then `AgentDefinition::validate` runs. It repeats checks that step 2 already made, so it is a backstop too.

The result is `LoadedPlugins`: the valid `definitions`, one `PluginLoadError` per rejected manifest, and one `PluginDiagnostic` per manifest either way. A load error's `kind_hint` is the bundle's directory name. The hook feed and `rimz agents check` use it to match a broken bundle to a kind.

`loaded()` runs the load once per process behind a `OnceLock` and logs one warning per rejected manifest. `registry::all_definitions()` chains `BUILTINS` with `loaded().definitions`, so a valid plugin reaches every consumer that resolves agents through the registry, with no plugin-specific branch. The authoring commands call `load_from_root` directly and get a fresh load.

The once-per-process load means a long-lived process keeps the plugin set it saw at its first registry read. A bundle that is added, fixed, or broken later reaches that process only after it restarts. Short-lived commands, such as each `rimz hooks feed` and each `rimz hooks apply` child, see the change at once.

A plugin's spec must outlive the process like a built-in's `static` spec, because `AgentDefinition` and `AgentSpec` hold `&'static` references. `build_adapter` therefore leaks the manifest, the bundle path, the spec, and every derived string and slice. The leak happens once per load, including for a manifest that step 4 rejects, so its size is bounded by machine configuration. Live manifest reload would need the registry to move to owned shared values, as the comment on `build_adapter` says.

## The derived spec

`build_descriptor` fills the `AgentSpec` from the manifest, and a plugin cannot set what the manifest has no field for. The values it fixes for every plugin:

| Spec field | Plugin value |
| --- | --- |
| `bin_names` | the kind alone |
| `aliases`, `sub_providers`, `expected_windows`, `tools.blocking` | empty |
| `plan_label` | `TitleCaseOnly` |
| `tool_rules`, `host_skills` | `Unsupported` |
| `capabilities.native_ask_ui`, `registers_lazily` | from `[capabilities]` |
| `capabilities.direct_account_usage` | true when an account probe is declared |
| `capabilities.transcript_tail_context`, `local_session_discovery`, `daemon_hooked_sessions`, `prompt_context`, `hook_context`, `remote_control` | off |
| `capabilities.same_pane_session` | `KeepPrimary` |
| `launch` | `LaunchSpec::EMPTY` without a `[launch]` table: no program, so `launch_command` returns `None`; the three `launch.*` rows below hold only with one |
| `launch.prompt` | positional after `--` |
| `launch.resume`, `fork`, `max_turn_flag`, `interrupt_key`, `presets.auto_compact` | none; resume argv comes from `resume_command` instead |
| `launch.compact_command` | the manifest string, with `CompactInstruction::Unsupported` |
| `default_model`, `default_context_window` | none |
| `thread_key` | `[transcripts].thread-key`, default `PerFile` |

The installation, runtime-control, session, and transcript capability traits are empty impls, and `skills_home` returns `None`. `launch_command` and `resume_command` resolve `argv[0]` with `probes::resolve_executable`, the same rule as probes: a path that contains a separator joins the bundle directory.

### Declared coverage

`derive_coverage`, `derive_lifecycle_hooks`, and `derive_user_coverage` compute both coverage matrices from `emits`, the capability flags, and which probes are declared. The published claims therefore follow the bundle and cannot drift from it. The reference's [coverage table](../../reference/agent-plugins.md#coverage-and-doctor) lists the condition for each concern. The code adds these details:

- A concern is `Wired` when its condition holds and `Unsupported` otherwise. `idle_notification` is the one exception: it is always `Partial`, through `turn_end` and the stall window.
- `answer`, `launch_reminders`, `background_parking`, `background_shells`, `hook_install`, `tool_stats`, and `remote_control` are always `Unsupported`. The `hook_install` reason names the bundle's setup doc.
- `context_usage` is the one concern that a capability flag (`context-usage`) can wire without a matching event.
- Each lifecycle-hook row is `Native` exactly when its canonical event is in `emits`. The exception is `lost`, which is always `Derived` through the `rimz exec` wrapper.
- Among the user-capability marks, `state` and `subagents` can reach `Full`. `live`, `history`, `account`, and `ask` are `Partial` at best, because what lands depends on what the plugin sends.

[adapter.md#declared-coverage](./adapter.md#declared-coverage) defines both matrices.

`emits` constrains claims, never ingest: a valid canonical event missing from `emits` still decodes.

## Hook ingest

The shim runs `rimz hooks feed --source <kind> --event <event>` with one envelope on stdin. The feed treats a plugin like any adapter ([adapter.md#the-feed](./adapter.md#the-feed)): it reads and parses the payload, appends a raw frame to the room's ingress log, and prints the reply. A plugin inherits the silent `payload_hook_reply`, so hook stdout stays empty. The envelope is decoded later, in the `rimz hooks apply` child that `process_frame` in [`cli/hooks.rs`](../../../crates/rimz/src/cli/hooks.rs) runs, through `PluginAdapter::decode_hook`.

`decode_hook` runs these steps:

1. Parse. `Envelope::parse` deserializes the payload once. It fails on a shape serde rejects (including a known event with malformed typed fields), a `protocol` other than 1, or a `hook_event_name` that differs from the feed's event name. A failure logs at debug level and returns class `Unknown` with no observation.
2. Check `emits`. An event name absent from `emits` logs `warn_undeclared_once`, once per kind and event per process. A name outside `CANONICAL_EVENTS` parses as `CanonicalEvent::Unknown` and can never be in `emits`, so it takes this warning before it returns class `Unknown` with no signal.
3. Classify tools. A `tool_use` counts as mutating when `is_error` is false and `tool_name` is in the spec's mutating tools, and as editing when the name is also in the editing tools.
4. Normalize. `CanonicalEvent::normalize` maps the event to its class, ask kind, `LifecycleSignal`, ask questions, turn error, final message, and the progress and session-ended flags.
5. Route. The route key is `agent_id`, falling back to `session_id`. The session is `session_id`, and `cwd` is the worktree.
6. Attach context. A `context` event attaches a `ContextObservation`. `normalize_context` stamps `source` and `observed_at`, and folds top-level `model`, `total_cost_usd`, `rate_limits`, and token fields into the `AgentContext` shape where the record's own fields are absent. The observation needs a root identity, so a context envelope without a `session_id`, or with an `agent_id` that differs from it, is dropped. A `context` event carries no lifecycle signal and stops here.
7. Resolve identity. The shared helpers in [`agents/identity.rs`](../../../crates/rimz/src/agents/identity.rs) decide. `subagent_start` and `subagent_end` need a child `agent_id` distinct from the parent `session_id`, and anything else is quarantined with an error on the `rimz::agent::lifecycle` target. A root event whose `agent_id` differs from its `session_id` is dropped as a foreign child.
8. Observe. The `AgentLifecycleObservation` carries model, effort, usage (`context_pct` capped at 100), the transcript path, and the `turn_start` prompt as both task and prompt.

`native-ask-ui` decides what an `awaiting_input` does. The event always decodes as `AwaitingUser`, but `process_frame` records it only when the spec declares `native_ask_ui`. With the flag off, the raw frame still reaches the ingress log like any other, but its apply records nothing: no waiting state, no transcript ask, and no usage from that envelope. No native prompt exists for a waiting row to route the user to. Every built-in declares the flag, so this branch runs only for a plugin. [adapter.md#two-channels](./adapter.md#two-channels) describes the ask channel the flag gates, and the ask coverage rows go `Unsupported` in the same case.

`rimz hooks feed` is the only delivery path that protocol 1 has. The envelope carries nothing specific to process execution, so the vocabulary does not depend on it.

## Probes

Probes are pull-only enrichment. Every probe goes through `run_json_diagnostic` in `probes.rs`, which enforces the limits that the reference's [probe contracts](../../reference/agent-plugins.md#probe-contracts) publish: `PROBE_TIMEOUT` (3 s), `MAX_STDOUT` (1 MiB), and `MAX_STDERR` (16 KiB). Three properties of the runner are invisible from the contract:

- It is synchronous, and the 3-second deadline bounds only the wait for exit. The runner writes the spend request first, then polls `try_wait` every 10 ms (`POLL_INTERVAL`) and kills the child at the deadline. After the exit it joins the two pipe readers with no bound, so a probe that blocks on its request or leaves a descendant holding a pipe open can hold the calling thread past 3 seconds.
- The child inherits the RimZ process environment. Every `PluginAdapter` method that takes a login environment ignores it, so the probes and the transcript globs read the same files whatever login a session runs under.
- A failure logs a warning and yields the method's empty result, never an error. The causes are a spawn error, a failed request write, a timeout, a nonzero exit, oversized stdout, and, for spend and account, invalid JSON. The version probe's stdout is plain text.

| Probe | Adapter method | Maps to | Empty result |
| --- | --- | --- | --- |
| spend | `SpendingCapability::parse_spend` | a `CachedEntry` per entry with an RFC 3339 timestamp; `cost_usd` taken verbatim (non-finite becomes 0) and the `PriceBook` ignored; `SpendCursor.state` round-trips the plugin's opaque cursor and `SpendCursor.offset` is the file length at parse time; `origin` passes through; the update is `SpendUpdate::Extend` | `SpendParse::default()` |
| account | `AccountCapability::probe_account`, `probe_account_usage` | `AgentAccount` with plan and account id; usage adds `rate_limit_windows` and keys the account by `account_id`; `logged_out` or an all-empty response is `LoggedOut` or `NoCredentials` | `Unavailable` and `Failed`; with no account probe declared, `LoggedOut` and `Unsupported` |
| version | `LaunchCapability::probe_version` | the first non-empty stdout line | `None` |

Spend discovery needs both halves. `spending_sources` returns sources only when the manifest has `[transcripts]` and a spend probe, while `transcript_files` lists the glob matches from `[transcripts]` alone. A glob that starts with `~/` expands from the process `HOME`, an absolute glob stands as written, and a relative glob resolves from the bundle directory.

Declaring an account probe sets `direct_account_usage` without running the probe. The probe runs only when a consumer asks for the account. The account wire does not supply `sub_provider`, so `AccountCapability::has_quota_surface` keeps its `false` default.

## Invalid manifests at each entry point

A broken bundle warns at room entry and is absent from the agent registry:

| Entry point | Behaviour |
| --- | --- |
| Managed room entry | [`room/judgment.rs`](../../../crates/rimz/src/room/judgment.rs) records one warning per rejected manifest, which names `rimz agents register --check`, and the room opens; launching the rejected kind refuses it as unknown |
| Registry reads through `all_definitions()` | see valid plugins only; `loaded()` warned once per rejected manifest |
| `rimz hooks feed --source <kind>` | when the kind is not a definition and a load error's `kind_hint` matches, the feed warns and exits 0 with empty stdout, before it reads stdin |
| `rimz doctor` | one row per `PluginDiagnostic`: validity, error, setup doc, and each probe's presence and executable bit |

## Authoring commands

`rimz agents register <kind>` ([`cli/agents_cmd/register.rs`](../../../crates/rimz/src/cli/agents_cmd/register.rs)) checks the kind grammar and rejects a kind that `all_definitions()` already knows (a built-in or a loaded plugin) or whose directory exists. It then creates the bundle directory and writes `agent.toml`, `README.md`, `shim.sh`, `probes/spend`, and `probes/account`, each file atomically, and marks the shim and both probes executable. A failed write removes the directory. `register --check` runs a fresh `load_from_root` over the whole `agents.d` and exits nonzero listing every error.

`rimz agents check <kind>` calls `check_from_root`. It refuses a built-in kind, loads fresh, and fails with the manifest error when that kind was rejected. The report counts both coverage matrices and checks every declared probe for presence and the executable bit. It runs the account and version probes for real, and the spend probe only with `--spend-file`.

`--replay <jsonl>` replays envelopes without opening a store. It parses each line with `Envelope::parse_diagnostic`, which returns the reason that `parse` hides, decodes it through `decode_hook`, and steps a per-agent in-memory state with `agents::step`. A line counts as rejected when it is not JSON, has no string `hook_event_name`, fails to parse, yields no observation, or has no agent identity. An event missing from `emits` is a warning, and an unknown event is a row with no state change. Any rejection or failed probe fails the command.

The replay stops at the decode, so it does not apply the `native_ask_ui` gate that `process_frame` applies. With the flag off, a replayed `awaiting_input` still shows the agent waiting, while the live feed records nothing.

## See also

- [agent-plugins.md](../../reference/agent-plugins.md): the bundle, manifest, envelope, and probe contracts.
- [adapter.md](./adapter.md): the registry, the spec, the hook path, and declared coverage.
- [instances.md](./instances.md): how a registered session binds to its pane.
- [providers.md](./providers.md): the account and spend consumers that the probes feed.

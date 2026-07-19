# Agent plugin internals

`agents/adapters/plugin/` turns machine-tier bundle manifests into process-lifetime `AgentDefinition` values. The process boundary is the runtime: agent-owned shims push canonical hook JSON, and optional short-lived executables pull spend, account, and version enrichment.

## Load and registry

[`load.rs`](../../../crates/rimz/src/agents/adapters/plugin/load.rs) scans `$XDG_CONFIG_HOME/rimz/agents.d/*/agent.toml`, sorts paths for deterministic display order, validates collisions and bundle paths, and caches the result in a `OnceLock`. It returns valid adapters, structured load errors, and doctor diagnostics in one pass.

[`registry.rs`](../../../crates/rimz/src/agents/registry.rs) keeps the built-in `BUILTINS` slice for built-in hook management and exposes `all_definitions()` for kind resolution, coverage, doctor, presence, launches, and spend discovery. A valid plugin therefore reaches the lifecycle reducer, sidebar, target resolver, layout grammar, messaging, and provider dashboard through the existing adapter seam.

Definition capability objects use `&'static` references. Loading leaks each validated manifest, spec, adapter, and derived table once for the process lifetime. The allocation is bounded by machine configuration; live manifest reload is the threshold for replacing it with owned shared registry values.

## Derived declarations

[`manifest.rs`](../../../crates/rimz/src/agents/adapters/plugin/manifest.rs) makes `emits` the source of truth for native lifecycle coverage. `PluginAdapter` derives every `IntegrationConcern` and `LifecycleSignalKind` row from emitted-event declarations, capabilities, and probe presence, so a plugin author cannot hand-write a greener matrix than the bundle implements.

The spec owns leaked strings for branding, tools, process names, activity events, setup guidance, and launch facts. Hook installation remains self-managed and points at `setup-doc`; undeclared capabilities stay absent.

## Ingest and probes

[`protocol.rs`](../../../crates/rimz/src/agents/adapters/plugin/protocol.rs) is the structured protocol-1 parser. `PluginAdapter::decode_hook` parses each envelope once, checks its event against the feed event, resolves root and child identities through the shared identity helpers, and returns typed routing, lifecycle, question/final text, identity-bearing context/error evidence, and neutral output together. Context replay uses the normalized `ContextObservation` session ID rather than reparsing envelope identity. Valid canonical events omitted from `emits` still ingest with one warning per kind/event; unknown events drop at debug level, and malformed child identity quarantines through the normal lifecycle diagnostic target.

The canonical vocabulary is transport-independent. `rimz hooks feed` supplies the version-1 process delivery path; a resident receiver can carry the same envelopes over a socket without a protocol change.

[`probes.rs`](../../../crates/rimz/src/agents/adapters/plugin/probes.rs) owns fresh piped stdio, the three-second deadline, bounded output, relative executable resolution, and failure warnings. Spend cursors keep the file offset in `SpendCursor.offset` and opaque plugin state in `SpendCursor.state`; priced entries bypass `PriceBook`.

## Entry-point policy

Room start reads the cached error list before creating configuration or mux state and refuses when any manifest is invalid. Registry-driven read paths retain only valid adapters and warn once for each skipped manifest. Hook feed recognizes a source whose directory failed to load and returns neutral success rather than breaking the agent's hook path.

`rimz agents register` writes an atomically-created scaffold under the machine configuration root. `register --check` runs the same loader validation across the machine registry without entering a room. `rimz agents check <kind>` adds probe dry-runs and diagnostic envelope replay for plugin authors; replay calls the adapter seam and the pure lifecycle state machine without writing the store. `rimz doctor` renders valid and invalid diagnostics, setup guidance, and probe file status.

# Off-box error reporting

Off-box error reporting routes Rimz diagnostics to a Sentry project so an operator watches a fleet's health without tailing every box. It captures the warnings and errors Rimz raises, the panics it hits, and the agent conditions it observes — rate limits, provider overload, and other turn-ending API failures — and reports the agent-generated ones at warning level. Reporting is best-effort enrichment: it never holds a correctness path, and it is dormant until you opt in.

## Opting in

Reporting turns on when a DSN resolves, from `RIMZ_SENTRY_DSN` or the per-machine `[sentry]` config; the env value wins, and an empty value counts as unset. The DSN lives per-machine — never in the committed `.rimz/config.toml` — so a clone or pull cannot redirect a contributor's telemetry, and the DSN stays off the [project trust surface](../sidebar/trust.md). `RIMZ_SENTRY_ENVIRONMENT` (or `[sentry] environment`, default `production`) tags the deployment. The config shape lives in [configuration.md → Off-Box Error Reporting](../../reference/configuration.md#off-box-error-reporting); the data boundary lives in [security.md → Off-box error reporting](../../guide/security.md#off-box-error-reporting).

With no DSN, no client is created and Rimz makes no network calls.

## One init point covers every process

`main` creates the Sentry client once, before the tracing subscriber, and holds the guard for the whole process; the guard flushes pending events on drop. Every Rimz subcommand runs through that one `main` — the CLI, the `hooks feed` subprocess where agent conditions are observed, and the `sidebar serve` loop — so a single init covers them all. The wasm presence plugin is a separate binary with no HTTP stack and reports nothing.

`observability::init` resolves the DSN, parses it, and returns a [`Reporting`](../../../crates/rimz/src/observability.rs) the binary holds. A malformed DSN yields `Reporting::InvalidDsn`, logged with the fix once the subscriber is live and otherwise inert — a telemetry typo never degrades or blocks a command. A live workspace pin (`RIMZ_WORKSPACE_ID`) becomes a `workspace` scope tag, so one machine-wide DSN still filters per repository.

## The tracing bridge is the capture path

Rimz already speaks diagnostics through `tracing`. The Sentry layer ([`sentry_tracing_layer`](../../../crates/rimz/src/observability.rs)) joins the subscriber alongside the stderr formatter and turns each `warn!`/`error!` into a Sentry event whose level mirrors the tracing level — `warn!` to warning, `error!` to error. The env filter is per-layer on the stderr sink, so the sidebar's `off` silences stderr without gating capture; the Sentry layer carries its own `WARN` filter, which keeps the global max-level hint at `WARN` so hot paths never construct lower events. When no DSN resolves the layer is omitted entirely, and behaviour is byte-for-byte the prior subscriber.

Agent-generated conditions ride the same path. When the hook lifecycle merges a fresh turn-error marker — `merge_turn_error_marker` in [`cli/hooks/lifecycle.rs`](../../../crates/rimz/src/cli/hooks/lifecycle.rs) reports the merge changed state — it emits one `warn!` under the `rimz::agent::turn_error` target carrying the agent kind and the [`TurnErrorClass`](../../../crates/rimz/src/agents/context.rs) (`PausedRateLimit`, `PausedOverloaded`, or `Failed`). Gating on the transition keeps it to one event per condition rather than one per poll, and the warning level marks it as observed-not-Rimz-fault.

## What stays off the wire

Personal data is off by default and the hostname is stripped in `before_send`. Events carry Rimz error text, the file paths that appear in those errors, the agent kind and turn-error class, and the `workspace` tag; hook payloads, prompts, and transcripts are never forwarded. A network failure is swallowed by the transport — the same small rustls-backed `ureq` client Rimz uses for pricing — and never surfaces on a Rimz path.

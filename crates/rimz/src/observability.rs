//! Off-box error reporting (Sentry).
//!
//! Reporting is off unless a DSN resolves from the `RIMZ_SENTRY_DSN` env or the
//! per-machine `[sentry]` config. When on, [`init`] returns a guard the binary
//! holds for the process lifetime — it flushes pending events on drop, which
//! covers the short-lived hook subprocesses — and [`sentry_tracing_layer`]
//! bridges the `tracing` subscriber so every Rimz `warn!`/`error!`, including
//! the agent turn-error warning under the `rimz::agent::turn_error` target,
//! becomes a Sentry event (warning / error level mirrors the tracing level).
//!
//! Events arrive with debug context: [`set_command_scope`] tags the scope with
//! the running command, build id, and (when a process serves one) the agent and
//! session, plus a structured `rimz` context grouping the same facts; callsites
//! add a searchable `tags.operation` and pass the error as `&dyn Error` so a
//! stacktrace and exception attach. `info!` lines become breadcrumbs, so a
//! warning arrives with the trail that led to it.
//!
//! Reporting is best-effort enrichment, not a precondition: a malformed DSN
//! logs the fix and stays off, and a network failure never blocks a Rimz path.
//! The hostname is withheld and PII is off by default — the telemetry surface
//! is documented in [`docs/guide/security.md`](../../docs/guide/security.md).

use std::sync::Arc;

use sentry::ClientInitGuard;
use sentry_tracing::EventFilter;
use tracing::{Level, Metadata};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::registry::LookupSpan;

use crate::config::MachineConfig;
use crate::workspace::ENV_WORKSPACE_ID;

const ENV_DSN: &str = "RIMZ_SENTRY_DSN";
const ENV_ENVIRONMENT: &str = "RIMZ_SENTRY_ENVIRONMENT";
const DEFAULT_ENVIRONMENT: &str = "production";

/// Outcome of [`init`], held by the binary for the process lifetime.
#[must_use = "drop flushes pending events; hold the guard for the process lifetime"]
pub enum Reporting {
    /// No DSN configured — Sentry is off and no client exists.
    Off,
    /// A DSN resolved and the client initialized.
    On(ClientInitGuard),
    /// A DSN resolved but failed to parse; reporting stays off. The message is
    /// logged by [`Reporting::report`] once the subscriber is live.
    InvalidDsn(String),
}

impl Reporting {
    /// Whether a Sentry client is active — drives whether the binary attaches
    /// [`sentry_tracing_layer`] to the subscriber.
    pub fn enabled(&self) -> bool {
        matches!(self, Self::On(_))
    }

    /// Surface a malformed-DSN misconfiguration. Called after the tracing
    /// subscriber is installed so the error reaches stderr / state logs.
    pub fn report(&self) {
        if let Self::InvalidDsn(error) = self {
            tracing::error!(
                target: "rimz::observability",
                error = %error,
                "invalid Sentry DSN; reporting disabled — fix [sentry] dsn in ~/.config/rimz/config.toml or set RIMZ_SENTRY_DSN",
            );
        }
    }
}

/// Initialize Sentry when a DSN resolves. Best-effort: a config that fails to
/// load is treated as absent (the invoked command surfaces config errors), and
/// a DSN that fails to parse yields [`Reporting::InvalidDsn`] rather than a
/// panic or a degraded surface.
pub fn init() -> Reporting {
    let config = MachineConfig::load().unwrap_or_default();
    let Some((dsn, environment)) = resolve_from(
        env_nonempty(ENV_DSN),
        env_nonempty(ENV_ENVIRONMENT),
        &config,
    ) else {
        return Reporting::Off;
    };
    let dsn = match dsn.parse::<sentry::types::Dsn>() {
        Ok(dsn) => dsn,
        Err(err) => return Reporting::InvalidDsn(err.to_string()),
    };
    let guard = sentry::init(sentry::ClientOptions {
        dsn: Some(dsn),
        release: sentry::release_name!(),
        environment: Some(environment.into()),
        // External reporting withholds personal data by default; strip the
        // hostname the contexts integration would otherwise attach.
        send_default_pii: false,
        // Attach a stacktrace to error/warning events that carry an exception
        // (a callsite's `error = &err as &dyn Error`) so a report names where it
        // came from, not just what failed. Free with the `backtrace` feature.
        attach_stacktrace: true,
        before_send: Some(Arc::new(|mut event: sentry::protocol::Event<'static>| {
            event.server_name = None;
            Some(event)
        })),
        ..Default::default()
    });
    tag_scope();
    Reporting::On(guard)
}

/// Tag the active scope with the pinned workspace so a machine-global DSN still
/// filters per project in Sentry.
fn tag_scope() {
    if let Some(workspace) = env_nonempty(ENV_WORKSPACE_ID) {
        sentry::configure_scope(|scope| scope.set_tag("workspace", workspace));
    }
}

/// Low-cardinality facts the cli layer knows after parsing the command line.
/// Values stay free of arguments and free-form text so they make stable Sentry
/// facets.
pub struct ScopeFacts<'a> {
    /// The resolved command, e.g. `"sidebar serve"` or `"codex refresh-context"`.
    pub command: &'a str,
    /// The agent session the process acts on, when exactly one is known.
    pub session: Option<&'a str>,
    /// The agent kind the process serves, when the command implies one.
    pub agent: Option<&'a str>,
}

/// Tag the active scope with the command and build id and attach a structured
/// `rimz` context grouping the command, build, session, and agent, so every
/// event this process reports inherits them. The hub is process-global, so one
/// call near dispatch covers the long-lived `sidebar serve` and each short-lived
/// hook subprocess alike.
pub fn set_command_scope(facts: ScopeFacts<'_>) {
    use sentry::protocol::{Context, Value};

    let build = crate::build_id::current_if_ready();
    sentry::configure_scope(|scope| {
        scope.set_tag("command", facts.command);
        if let Some(build) = build {
            scope.set_tag("build", build);
        }
        let mut ctx = std::collections::BTreeMap::<String, Value>::new();
        ctx.insert("command".into(), facts.command.into());
        if let Some(build) = build {
            ctx.insert("build".into(), build.into());
        }
        if let Some(session) = facts.session {
            ctx.insert("session".into(), session.into());
        }
        if let Some(agent) = facts.agent {
            ctx.insert("agent".into(), agent.into());
        }
        scope.set_context("rimz", Context::Other(ctx));
    });
}

/// The target a deliberate breadcrumb seed emits under. Only an `info!` on this
/// target becomes a Sentry breadcrumb, so the trail is the curated set of
/// cold-path steps — never an arbitrary `info!` field (a socket path, a cwd)
/// the privacy boundary keeps off-box.
pub const BREADCRUMB_TARGET: &str = "rimz::trail";

/// The Sentry bridge layer: `warn!`/`error!` become Sentry events and an `info!`
/// on [`BREADCRUMB_TARGET`] becomes a breadcrumb attached to the next event, so
/// a warning arrives with the trail that led to it. The `INFO` level filter
/// keeps the global max-level hint at `INFO` — `debug!`/`trace!` are still never
/// constructed — so the breadcrumb trail stays a cold-path concern (the
/// `sidebar serve` hot loop emits no `info!`).
pub fn sentry_tracing_layer<S>() -> impl Layer<S>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    sentry_tracing::layer()
        .event_filter(event_filter)
        .with_filter(LevelFilter::INFO)
}

fn event_filter(metadata: &Metadata<'_>) -> EventFilter {
    classify(*metadata.level(), metadata.target())
}

/// `warn!`/`error!` map to Sentry events (the event level mirrors the tracing
/// level); an `info!` on [`BREADCRUMB_TARGET`] maps to a breadcrumb; everything
/// else is ignored, so an unmarked `info!` never carries its fields off-box.
fn classify(level: Level, target: &str) -> EventFilter {
    match level {
        Level::ERROR | Level::WARN => EventFilter::Event,
        Level::INFO if target == BREADCRUMB_TARGET => EventFilter::Breadcrumb,
        _ => EventFilter::Ignore,
    }
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

/// Resolve `(dsn, environment)` with env overriding the per-machine config.
/// Empty strings count as unset; environment defaults to `production`.
fn resolve_from(
    env_dsn: Option<String>,
    env_environment: Option<String>,
    config: &MachineConfig,
) -> Option<(String, String)> {
    let dsn = env_dsn
        .or_else(|| config.sentry.dsn.clone())
        .filter(|value| !value.is_empty())?;
    let environment = env_environment
        .or_else(|| config.sentry.environment.clone())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_ENVIRONMENT.to_owned());
    Some((dsn, environment))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(dsn: Option<&str>, environment: Option<&str>) -> MachineConfig {
        let mut config = MachineConfig::default();
        config.sentry.dsn = dsn.map(ToOwned::to_owned);
        config.sentry.environment = environment.map(ToOwned::to_owned);
        config
    }

    #[test]
    fn unset_dsn_resolves_to_none() {
        assert!(resolve_from(None, None, &MachineConfig::default()).is_none());
        // An empty config value is also "off".
        assert!(resolve_from(None, None, &config_with(Some(""), None)).is_none());
    }

    #[test]
    fn config_dsn_defaults_environment_to_production() {
        let (dsn, environment) = resolve_from(
            None,
            None,
            &config_with(Some("https://k@o1.ingest.sentry.io/2"), None),
        )
        .expect("dsn resolves");
        assert_eq!(dsn, "https://k@o1.ingest.sentry.io/2");
        assert_eq!(environment, DEFAULT_ENVIRONMENT);
    }

    #[test]
    fn env_overrides_config_dsn_and_environment() {
        let config = config_with(Some("https://config@o1.ingest.sentry.io/2"), Some("prod"));
        let (dsn, environment) = resolve_from(
            Some("https://env@o9.ingest.sentry.io/9".to_owned()),
            Some("dev".to_owned()),
            &config,
        )
        .expect("dsn resolves");
        assert_eq!(dsn, "https://env@o9.ingest.sentry.io/9");
        assert_eq!(environment, "dev");
    }

    #[test]
    fn config_environment_used_when_env_absent() {
        let config = config_with(Some("https://k@o1.ingest.sentry.io/2"), Some("staging"));
        let (_, environment) = resolve_from(None, None, &config).expect("dsn resolves");
        assert_eq!(environment, "staging");
    }

    #[test]
    fn classify_maps_levels_and_gates_breadcrumbs_to_the_trail_target() {
        // WARN/ERROR are events regardless of target.
        assert!(classify(Level::ERROR, "rimz::anything").contains(EventFilter::Event));
        assert!(classify(Level::WARN, "rimz::anything").contains(EventFilter::Event));
        // INFO is a breadcrumb only on the dedicated trail target.
        let trail = classify(Level::INFO, BREADCRUMB_TARGET);
        assert!(trail.contains(EventFilter::Breadcrumb));
        assert!(!trail.contains(EventFilter::Event));
        // An unmarked INFO (e.g. a broker socket-path log) is ignored — its
        // fields never ride off-box as breadcrumb data. (`EventFilter::Ignore`
        // is the empty flag set.)
        assert!(classify(Level::INFO, "rimz::agents::codex::broker").is_empty());
        // DEBUG/TRACE are always ignored, even on the trail target.
        assert!(classify(Level::DEBUG, BREADCRUMB_TARGET).is_empty());
        assert!(classify(Level::TRACE, "rimz::anything").is_empty());
    }

    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use tracing_subscriber::layer::SubscriberExt;

    /// Records every captured event so a test can assert what reached Sentry.
    struct Recorder {
        events: Arc<Mutex<Vec<sentry::protocol::Event<'static>>>>,
    }

    impl sentry::Transport for Recorder {
        fn send_envelope(&self, envelope: sentry::Envelope) {
            for item in envelope.items() {
                if let sentry::protocol::EnvelopeItem::Event(event) = item {
                    self.events
                        .lock()
                        .expect("recorder lock")
                        .push(event.clone());
                }
            }
        }
    }

    // Sentry's hub is process-global; nextest isolates each test in its own
    // process, so installing a client here does not leak into sibling tests.
    #[test]
    fn warn_and_error_route_to_sentry_at_matching_levels() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorder: Arc<Recorder> = Arc::new(Recorder {
            events: events.clone(),
        });
        let _guard = sentry::init(sentry::ClientOptions {
            dsn: Some(
                "https://public@example.com/1"
                    .parse()
                    .expect("test dsn parses"),
            ),
            transport: Some(Arc::new(recorder)),
            ..Default::default()
        });

        let subscriber = tracing_subscriber::registry().with(sentry_tracing_layer());
        tracing::subscriber::with_default(subscriber, || {
            // INFO on the trail target is a breadcrumb: it rides along on the
            // next captured event rather than becoming an event of its own.
            tracing::info!(target: BREADCRUMB_TARGET, "about to end the turn");
            // Stands in for the agent turn-error warning emitted by the hook
            // lifecycle — agent-generated, reported at warning level.
            tracing::warn!(
                target: "rimz::agent::turn_error",
                class = "PausedRateLimit",
                "agent turn ended on a provider error",
            );
            tracing::error!("a rimz failure");
        });
        sentry::Hub::current()
            .client()
            .expect("client installed")
            .flush(Some(Duration::from_secs(1)));

        let captured = events.lock().expect("recorder lock");
        let levels: Vec<sentry::Level> = captured.iter().map(|event| event.level).collect();
        assert_eq!(
            levels,
            vec![sentry::Level::Warning, sentry::Level::Error],
            "agent warning then rimz error, info is a breadcrumb not an event",
        );
        let warning = &captured[0];
        assert!(
            warning
                .breadcrumbs
                .values
                .iter()
                .any(|crumb| crumb.message.as_deref() == Some("about to end the turn")),
            "the info line rode along on the warning as a breadcrumb",
        );
    }

    // Sentry's hub is process-global; nextest isolates each test in its own
    // process, so the scope set here does not leak into sibling tests.
    #[test]
    fn enriched_scope_and_fields_reach_sentry() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorder: Arc<Recorder> = Arc::new(Recorder {
            events: events.clone(),
        });
        let _guard = sentry::init(sentry::ClientOptions {
            dsn: Some(
                "https://public@example.com/1"
                    .parse()
                    .expect("test dsn parses"),
            ),
            transport: Some(Arc::new(recorder)),
            attach_stacktrace: true,
            ..Default::default()
        });

        set_command_scope(ScopeFacts {
            command: "codex refresh-context",
            session: Some("ses_x"),
            agent: Some("codex"),
        });

        let subscriber = tracing_subscriber::registry().with(sentry_tracing_layer());
        tracing::subscriber::with_default(subscriber, || {
            let err = std::io::Error::other("boom");
            tracing::warn!(
                tags.operation = "codex.oauth_usage",
                error = &err as &dyn std::error::Error,
                "codex OAuth usage fetch failed",
            );
        });
        sentry::Hub::current()
            .client()
            .expect("client installed")
            .flush(Some(Duration::from_secs(1)));

        let captured = events.lock().expect("recorder lock");
        let event = captured
            .iter()
            .find(|event| event.level == sentry::Level::Warning)
            .expect("warning captured");
        // Scope tag for the command; `tags.`-prefixed field promoted to a tag.
        assert_eq!(
            event.tags.get("command").map(String::as_str),
            Some("codex refresh-context")
        );
        assert_eq!(
            event.tags.get("operation").map(String::as_str),
            Some("codex.oauth_usage")
        );
        // `error = &dyn Error` attaches an exception (and, with attach_stacktrace, a stack).
        assert!(
            !event.exception.values.is_empty(),
            "dyn Error attaches an exception"
        );
        // The structured `rimz` context carries session and agent.
        match event.contexts.get("rimz") {
            Some(sentry::protocol::Context::Other(map)) => {
                assert_eq!(
                    map.get("session").and_then(|value| value.as_str()),
                    Some("ses_x")
                );
                assert_eq!(
                    map.get("agent").and_then(|value| value.as_str()),
                    Some("codex")
                );
            }
            other => panic!("missing or unexpected rimz context: {other:?}"),
        }
    }
}

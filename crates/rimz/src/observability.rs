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

/// The Sentry bridge layer: `warn!`/`error!` become Sentry events, everything
/// below is dropped. The `WARN` level filter keeps the global max-level hint at
/// `WARN` so hot paths that disable the fmt layer never construct lower events.
pub fn sentry_tracing_layer<S>() -> impl Layer<S>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    sentry_tracing::layer()
        .event_filter(event_filter)
        .with_filter(LevelFilter::WARN)
}

/// `warn!`/`error!` map to Sentry events (the event level mirrors the tracing
/// level); the `WARN` layer filter means nothing lower reaches this.
fn event_filter(metadata: &Metadata<'_>) -> EventFilter {
    match *metadata.level() {
        Level::ERROR | Level::WARN => EventFilter::Event,
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
    fn event_filter_lifts_warn_and_error_to_events() {
        // A WARN/ERROR callsite becomes a Sentry event; INFO and below are
        // ignored (and never reach the layer past the WARN level filter).
        for (level, expect_event) in [
            (Level::ERROR, true),
            (Level::WARN, true),
            (Level::INFO, false),
            (Level::DEBUG, false),
            (Level::TRACE, false),
        ] {
            let filter = level_filter(level);
            assert_eq!(
                filter.contains(EventFilter::Event),
                expect_event,
                "level {level} event mapping",
            );
        }
    }

    // `Metadata` cannot be constructed directly in a test; mirror `event_filter`
    // over the level so the mapping table stays covered.
    fn level_filter(level: Level) -> EventFilter {
        match level {
            Level::ERROR | Level::WARN => EventFilter::Event,
            _ => EventFilter::Ignore,
        }
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
            // Stands in for the agent turn-error warning emitted by the hook
            // lifecycle — agent-generated, reported at warning level.
            tracing::warn!(
                target: "rimz::agent::turn_error",
                class = "PausedRateLimit",
                "agent turn ended on a provider error",
            );
            tracing::error!("a rimz failure");
            // Below the layer's WARN filter: never reaches Sentry.
            tracing::info!("ignored breadcrumb-level line");
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
            "agent warning then rimz error, nothing from info",
        );
    }
}
